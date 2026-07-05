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

/// ADR-0017 Phase 2 clustering (owned libp2p swarm) configuration.
///
/// Parsed from `WADDLE_CLUSTERING_*`. With `enabled` false (the default) the
/// swarm subsystem never starts and server behaviour is byte-for-byte
/// identical to the single-replica path. Clustering additionally requires the
/// `clustering` build feature and the Postgres control plane; see
/// [`crate::clustering`]. This struct carries no libp2p types (multiaddrs are
/// strings, parsed inside the feature-gated swarm module) so it compiles into
/// every build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusteringConfig {
    pub enabled: bool,
    /// libp2p listen multiaddrs for the swarm transport. Default: one
    /// ephemeral TCP address (`/ip4/0.0.0.0/tcp/0`).
    pub listen_addrs: Vec<String>,
    /// Kubernetes headless-Service peer discovery. `None` = cold start with no
    /// bootstrap peers (kademlia bootstrap retries continuously; an empty peer
    /// set is tolerated, avoiding cold-start deadlock).
    pub bootstrap: Option<ClusteringBootstrapConfig>,
    /// kameo `messaging::Config` limits and the ADR element-5 timeout
    /// hierarchy.
    pub messaging: ClusteringMessagingConfig,
}

/// Headless-Service peer discovery inputs for the swarm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusteringBootstrapConfig {
    /// Headless Service DNS name resolved (A/AAAA) to peer addresses.
    pub dns_name: String,
    /// TCP port peers listen on for the swarm transport.
    pub port: u16,
}

/// kameo `messaging::Config` limits plus the ADR element-5 timeout hierarchy.
///
/// Invariants enforced at parse time: `reply_timeout <= request_timeout` and
/// `mailbox_timeout <= request_timeout`. `request_timeout` is the sender-side
/// libp2p transport cap (`with_request_timeout`) and is the binding bound; any
/// `reply_timeout` above it is dead configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusteringMessagingConfig {
    pub request_timeout: Duration,
    pub reply_timeout: Duration,
    pub mailbox_timeout: Duration,
    /// Cap on concurrent asks per peer connection (kameo default 100).
    pub max_concurrent_streams: usize,
    /// Max serialized request envelope bytes (kameo default 1 MiB).
    pub max_request_bytes: u64,
    /// Max serialized response envelope bytes (kameo default 10 MiB).
    pub max_response_bytes: u64,
}

impl Default for ClusteringMessagingConfig {
    fn default() -> Self {
        Self {
            // Sized above the worst-case fenced-write / resume-handshake budget
            // (the 10s kameo default is too low per ADR element 5).
            request_timeout: Duration::from_secs(30),
            reply_timeout: Duration::from_secs(20),
            mailbox_timeout: Duration::from_secs(5),
            max_concurrent_streams: 256,
            max_request_bytes: 1024 * 1024,
            max_response_bytes: 10 * 1024 * 1024,
        }
    }
}

impl Default for ClusteringConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_addrs: vec!["/ip4/0.0.0.0/tcp/0".to_string()],
            bootstrap: None,
            messaging: ClusteringMessagingConfig::default(),
        }
    }
}

impl ClusteringConfig {
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

        let defaults = Self::default();
        let enabled = parse_bool_var(&vars, "WADDLE_CLUSTERING_ENABLED", false)?;

        let listen_addrs = match vars
            .get("WADDLE_CLUSTERING_LISTEN_ADDRS")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            None => defaults.listen_addrs,
            Some(raw) => {
                let addrs: Vec<String> = raw
                    .split(',')
                    .map(|entry| entry.trim().to_string())
                    .filter(|entry| !entry.is_empty())
                    .collect();
                if addrs.is_empty() {
                    return Err(
                        "WADDLE_CLUSTERING_LISTEN_ADDRS must contain at least one multiaddr"
                            .to_string(),
                    );
                }
                addrs
            }
        };

        let bootstrap = match vars
            .get("WADDLE_CLUSTERING_BOOTSTRAP_DNS")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            None => None,
            Some(dns_name) => {
                let port = parse_u64_var(&vars, "WADDLE_CLUSTERING_BOOTSTRAP_PORT", 7900)?;
                let port = u16::try_from(port).ok().filter(|value| *value != 0).ok_or_else(|| {
                    format!(
                        "WADDLE_CLUSTERING_BOOTSTRAP_PORT='{port}' must be a valid TCP port (1-65535)"
                    )
                })?;
                Some(ClusteringBootstrapConfig { dns_name, port })
            }
        };

        let messaging_defaults = ClusteringMessagingConfig::default();
        let request_timeout = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS",
            millis_u64(messaging_defaults.request_timeout),
        )?);
        let reply_timeout = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_REPLY_TIMEOUT_MS",
            millis_u64(messaging_defaults.reply_timeout),
        )?);
        let mailbox_timeout = Duration::from_millis(parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS",
            millis_u64(messaging_defaults.mailbox_timeout),
        )?);
        let max_concurrent_streams = parse_usize_var(
            &vars,
            "WADDLE_CLUSTERING_MAX_CONCURRENT_STREAMS",
            messaging_defaults.max_concurrent_streams,
        )?;
        let max_request_bytes = parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_MAX_REQUEST_BYTES",
            messaging_defaults.max_request_bytes,
        )?;
        let max_response_bytes = parse_u64_var(
            &vars,
            "WADDLE_CLUSTERING_MAX_RESPONSE_BYTES",
            messaging_defaults.max_response_bytes,
        )?;

        // ADR element-5 timeout hierarchy. `request_timeout` is the sender-side
        // transport cap; a `reply_timeout` above it is dead configuration (the
        // sender always observes `OutboundFailure(Timeout)` at the cap), and
        // the receiver-side `mailbox_timeout` must also fit under the cap.
        if reply_timeout > request_timeout {
            return Err(format!(
                "WADDLE_CLUSTERING_REPLY_TIMEOUT_MS ({}) must be <= \
                 WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS ({}): a reply timeout above the \
                 transport request timeout is dead configuration",
                reply_timeout.as_millis(),
                request_timeout.as_millis()
            ));
        }
        if mailbox_timeout > request_timeout {
            return Err(format!(
                "WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS ({}) must be <= \
                 WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS ({})",
                mailbox_timeout.as_millis(),
                request_timeout.as_millis()
            ));
        }
        if max_concurrent_streams == 0 {
            return Err("WADDLE_CLUSTERING_MAX_CONCURRENT_STREAMS must be at least 1".to_string());
        }
        if max_request_bytes == 0 || max_response_bytes == 0 {
            return Err(
                "WADDLE_CLUSTERING_MAX_REQUEST_BYTES and WADDLE_CLUSTERING_MAX_RESPONSE_BYTES \
                 must both be non-zero"
                    .to_string(),
            );
        }

        Ok(Self {
            enabled,
            listen_addrs,
            bootstrap,
            messaging: ClusteringMessagingConfig {
                request_timeout,
                reply_timeout,
                mailbox_timeout,
                max_concurrent_streams,
                max_request_bytes,
                max_response_bytes,
            },
        })
    }
}

/// Milliseconds of a `Duration` as `u64`, saturating (used only to derive
/// env-var defaults from the compiled-in `Duration` defaults).
fn millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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
    /// ADR-0017 Phase 2 clustering (owned libp2p swarm) configuration. With
    /// `enabled` false (the default) the swarm subsystem never starts.
    pub clustering: ClusteringConfig,
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

/// Typed validation failure for the `WADDLE_WS_KEEPALIVE_*` knobs
/// (issue #1090).
///
/// Per the typed-payloads rule, error results are typed enums; the
/// `Display` text is the human-facing startup diagnostic surfaced by
/// [`ServerConfig::from_env`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WsKeepaliveConfigError {
    /// The interval would let the worst-case inter-traffic gap
    /// (`2 × interval`) reach the gateway's ~300s stream-idle timeout.
    #[error(
        "WADDLE_WS_KEEPALIVE_INTERVAL_SECS='{value}' must be between 1 and {max}: the \
         worst-case inter-traffic gap is twice the interval and must stay under the \
         gateway's 300s stream-idle timeout"
    )]
    IntervalOutOfRange { value: u64, max: u64 },
    /// The miss limit is zero (would close every idle peer instantly)
    /// or so high that dead peers outlive the unacked-queue cap.
    #[error("WADDLE_WS_KEEPALIVE_MISS_LIMIT='{value}' must be between 1 and {max}")]
    MissLimitOutOfRange { value: u64, max: u64 },
    /// The env var is set but is not a base-10 unsigned integer.
    #[error("{key}='{value}' must be a positive integer")]
    NotAnInteger { key: &'static str, value: String },
}

/// Read a `WADDLE_WS_KEEPALIVE_*` var as `u64`, treating unset/blank
/// as the default. Sibling of [`parse_u64_var`] with a typed error.
fn ws_keepalive_u64_var(
    vars: &std::collections::HashMap<String, String>,
    key: &'static str,
    default: u64,
) -> Result<u64, WsKeepaliveConfigError> {
    match vars
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        None => Ok(default),
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| WsKeepaliveConfigError::NotAnInteger {
                key,
                value: raw.to_string(),
            }),
    }
}

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
) -> Result<waddle_xmpp::protocol::KeepaliveConfig, WsKeepaliveConfigError>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let vars = vars
        .into_iter()
        .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
        .collect::<std::collections::HashMap<_, _>>();
    let interval_secs = ws_keepalive_u64_var(&vars, "WADDLE_WS_KEEPALIVE_INTERVAL_SECS", 45)?;
    if !(1..=WS_KEEPALIVE_MAX_INTERVAL_SECS).contains(&interval_secs) {
        return Err(WsKeepaliveConfigError::IntervalOutOfRange {
            value: interval_secs,
            max: WS_KEEPALIVE_MAX_INTERVAL_SECS,
        });
    }
    let miss_limit = ws_keepalive_u64_var(&vars, "WADDLE_WS_KEEPALIVE_MISS_LIMIT", 2)?;
    if !(1..=WS_KEEPALIVE_MAX_MISS_LIMIT).contains(&miss_limit) {
        return Err(WsKeepaliveConfigError::MissLimitOutOfRange {
            value: miss_limit,
            max: WS_KEEPALIVE_MAX_MISS_LIMIT,
        });
    }
    Ok(waddle_xmpp::protocol::KeepaliveConfig {
        interval_ms: interval_secs * 1_000,
        // Infallible: miss_limit is range-checked to 1..=10 above.
        miss_limit: miss_limit as u32,
    })
}

/// Env-reading wrapper around [`ws_keepalive_from_vars`].
pub fn ws_keepalive_from_env(
) -> Result<waddle_xmpp::protocol::KeepaliveConfig, WsKeepaliveConfigError> {
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
            clustering: ClusteringConfig::default(),
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
        // `ServerConfig::from_env` predates the typed-error rule and
        // still aggregates `String` diagnostics; render the typed
        // keepalive error at this boundary.
        let ws_keepalive = ws_keepalive_from_env().map_err(|error| error.to_string())?;
        let spicedb = SpiceDbConfig::from_env()?;

        let occupant_id_secret =
            parse_occupant_id_secret(std::env::var("WADDLE_OCCUPANT_ID_SECRET").ok().as_deref())?;

        let clustering = ClusteringConfig::from_env()?;

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
            clustering,
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
            clustering: ClusteringConfig::default(),
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
            clustering: ClusteringConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clustering_defaults_are_disabled() {
        let config = ClusteringConfig::from_vars(std::iter::empty::<(&str, &str)>()).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.listen_addrs, vec!["/ip4/0.0.0.0/tcp/0".to_string()]);
        assert!(config.bootstrap.is_none());
        assert_eq!(config.messaging, ClusteringMessagingConfig::default());
        // Byte-for-byte-identical guarantee: the whole struct equals Default.
        assert_eq!(config, ClusteringConfig::default());
    }

    #[test]
    fn clustering_parses_enabled_and_listen_addrs() {
        let config = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_ENABLED", "true"),
            (
                "WADDLE_CLUSTERING_LISTEN_ADDRS",
                "/ip4/0.0.0.0/tcp/7900, /ip4/0.0.0.0/udp/7900/quic-v1",
            ),
        ])
        .unwrap();
        assert!(config.enabled);
        assert_eq!(
            config.listen_addrs,
            vec![
                "/ip4/0.0.0.0/tcp/7900".to_string(),
                "/ip4/0.0.0.0/udp/7900/quic-v1".to_string(),
            ]
        );
    }

    #[test]
    fn clustering_parses_bootstrap_dns_and_port() {
        let config = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_ENABLED", "1"),
            ("WADDLE_CLUSTERING_BOOTSTRAP_DNS", "waddle-server-swarm"),
            ("WADDLE_CLUSTERING_BOOTSTRAP_PORT", "7900"),
        ])
        .unwrap();
        let bootstrap = config.bootstrap.expect("bootstrap parsed");
        assert_eq!(bootstrap.dns_name, "waddle-server-swarm");
        assert_eq!(bootstrap.port, 7900);
    }

    #[test]
    fn clustering_bootstrap_defaults_port_when_dns_set() {
        let config = ClusteringConfig::from_vars([(
            "WADDLE_CLUSTERING_BOOTSTRAP_DNS",
            "waddle-server-swarm",
        )])
        .unwrap();
        assert_eq!(config.bootstrap.expect("bootstrap parsed").port, 7900);
    }

    #[test]
    fn clustering_rejects_zero_bootstrap_port() {
        let err = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_BOOTSTRAP_DNS", "swarm"),
            ("WADDLE_CLUSTERING_BOOTSTRAP_PORT", "0"),
        ])
        .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_BOOTSTRAP_PORT"));
    }

    #[test]
    fn clustering_rejects_reply_timeout_above_request_timeout() {
        // reply_timeout > request_timeout is dead configuration per ADR
        // element 5 (the sender caps out at the transport request_timeout).
        let err = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS", "10000"),
            ("WADDLE_CLUSTERING_REPLY_TIMEOUT_MS", "20000"),
        ])
        .unwrap_err();
        assert!(err.contains("must be <="));
    }

    #[test]
    fn clustering_rejects_mailbox_timeout_above_request_timeout() {
        let err = ClusteringConfig::from_vars([
            ("WADDLE_CLUSTERING_REQUEST_TIMEOUT_MS", "10000"),
            // Keep reply under the request cap so the mailbox check is the one
            // that trips (otherwise the reply-timeout invariant fires first).
            ("WADDLE_CLUSTERING_REPLY_TIMEOUT_MS", "5000"),
            ("WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS", "20000"),
        ])
        .unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_MAILBOX_TIMEOUT_MS"));
    }

    #[test]
    fn clustering_rejects_zero_concurrent_streams() {
        let err = ClusteringConfig::from_vars([("WADDLE_CLUSTERING_MAX_CONCURRENT_STREAMS", "0")])
            .unwrap_err();
        assert!(err.contains("MAX_CONCURRENT_STREAMS"));
    }

    #[test]
    fn clustering_rejects_non_boolean_enabled() {
        let err =
            ClusteringConfig::from_vars([("WADDLE_CLUSTERING_ENABLED", "maybe")]).unwrap_err();
        assert!(err.contains("WADDLE_CLUSTERING_ENABLED"));
    }

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
        for (bad, value) in [("0", 0), ("121", 121), ("300", 300)] {
            let err =
                ws_keepalive_from_vars([("WADDLE_WS_KEEPALIVE_INTERVAL_SECS", bad)]).unwrap_err();
            assert_eq!(
                err,
                WsKeepaliveConfigError::IntervalOutOfRange { value, max: 120 }
            );
            let rendered = err.to_string();
            assert!(
                rendered.contains("WADDLE_WS_KEEPALIVE_INTERVAL_SECS"),
                "diagnostic must name the env var; got: {rendered}"
            );
            assert!(
                rendered.contains("300s"),
                "diagnostic must explain the gateway constraint; got: {rendered}"
            );
        }
    }

    #[test]
    fn ws_keepalive_rejects_out_of_range_miss_limits() {
        for (bad, value) in [("0", 0), ("11", 11)] {
            let err =
                ws_keepalive_from_vars([("WADDLE_WS_KEEPALIVE_MISS_LIMIT", bad)]).unwrap_err();
            assert_eq!(
                err,
                WsKeepaliveConfigError::MissLimitOutOfRange { value, max: 10 }
            );
            assert!(
                err.to_string().contains("WADDLE_WS_KEEPALIVE_MISS_LIMIT"),
                "diagnostic must name the env var; got: {err}"
            );
        }
    }

    #[test]
    fn ws_keepalive_rejects_non_numeric_values() {
        let err =
            ws_keepalive_from_vars([("WADDLE_WS_KEEPALIVE_INTERVAL_SECS", "45s")]).unwrap_err();
        assert_eq!(
            err,
            WsKeepaliveConfigError::NotAnInteger {
                key: "WADDLE_WS_KEEPALIVE_INTERVAL_SECS",
                value: "45s".to_string()
            }
        );
        assert!(err.to_string().contains("must be a positive integer"));
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
