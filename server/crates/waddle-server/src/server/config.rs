use anyhow::Context;
use jid::{BareJid, DomainPart};
use std::path::PathBuf;
use std::str::FromStr;
use url::Url;

/// XMPP server configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct XmppConfig {
    /// Whether XMPP server is enabled (default: true)
    pub enabled: bool,
    /// XMPP server domain (default: "localhost")
    pub domain: String,
    /// Bare JID of the community spaces (XEP-0503) service for this
    /// deployment. Defaults to `spaces.<domain>`; overridable via
    /// `WADDLE_SPACES_JID` for deployments whose spaces component lives
    /// on a non-default subdomain.
    pub spaces_jid: BareJid,
    /// Domain of the MUC (XEP-0045) service for this deployment.
    /// Defaults to `muc.<domain>`; overridable via `WADDLE_MUC_DOMAIN`.
    pub muc_domain: DomainPart,
    /// MAM database URL (prefers dedicated XMPP DSN, otherwise the main runtime DSN)
    pub mam_database_url: Option<String>,
    /// Inbox database URL (prefers dedicated XMPP DSN, otherwise the main runtime DSN)
    pub inbox_database_url: Option<String>,
    /// Spaces-metadata database URL — prefers dedicated XMPP DSN, otherwise
    /// the main runtime DSN. Resolution order matches
    /// `resolve_xmpp_database_url`:
    /// `WADDLE_XMPP_SPACES_METADATA_DATABASE_URL` → `WADDLE_DATABASE_URL`.
    /// When unset, the store falls back to in-memory SQLite — suitable
    /// for tests only.
    pub spaces_metadata_database_url: Option<String>,
    /// Channel-space-link database URL — prefers dedicated XMPP DSN,
    /// otherwise the main runtime DSN. Resolution order:
    /// `WADDLE_XMPP_CHANNEL_SPACE_LINKS_DATABASE_URL` →
    /// `WADDLE_DATABASE_URL`. When unset, the store falls back to
    /// in-memory SQLite — suitable for tests only.
    pub channel_space_links_database_url: Option<String>,
    /// XEP-0160 offline-message (`pending_delivery`) database URL —
    /// prefers dedicated XMPP DSN, otherwise the main runtime DSN.
    /// Resolution order (matches `resolve_xmpp_database_url`):
    /// `WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL` →
    /// `WADDLE_DATABASE_URL`. When neither is set the storage falls
    /// back to in-memory SQLite — suitable only for tests; production
    /// deployments MUST set one of these env vars so queued offline
    /// DMs survive restart per issue #209.
    pub pending_delivery_database_url: Option<String>,
    /// XEP-0198 stream-management persistence database URL —
    /// prefers dedicated XMPP DSN, otherwise the main runtime DSN.
    /// Resolution order:
    /// `WADDLE_XMPP_SM_DATABASE_URL` → `WADDLE_DATABASE_URL`. When
    /// unset the storage falls back to in-memory SQLite — suitable
    /// for tests; production deployments MUST set one of these env
    /// vars so detached sessions survive restart per issue #209
    /// slice (d) Q8 = B.
    pub sm_database_url: Option<String>,
    /// PubSub/PEP database URL (prefers dedicated XMPP DSN, otherwise the main runtime DSN)
    pub pubsub_database_url: Option<String>,
    /// Trusted public RFC 7395 endpoint used by clients. Its scheme is the
    /// authoritative source for transport-security decisions; the unrelated
    /// HTTP application base URL and client-controlled forwarding headers are
    /// not.
    pub public_websocket_url: Url,
    /// Whether native JID authentication is enabled (default: true)
    /// When enabled, users can authenticate with SCRAM-SHA-256 using native credentials.
    pub native_auth_enabled: bool,
    /// ACME configuration for managed TLS certificates.
    pub acme: XmppAcmeConfig,
}

#[derive(Debug, Clone)]
pub struct XmppAcmeConfig {
    /// Whether ACME-managed certificates are enabled
    pub enabled: bool,
    /// Contact email for ACME account registration
    pub email: Option<String>,
    /// Cache directory for ACME account and certificate material
    pub cache_dir: PathBuf,
    /// Use Let's Encrypt production directory instead of staging
    pub production: bool,
}

impl Default for XmppConfig {
    fn default() -> Self {
        let domain = "localhost".to_string();
        let spaces_jid = derived_spaces_jid(&domain)
            .expect("default xmpp domain 'localhost' yields a valid spaces JID");
        let muc_domain = derived_muc_domain(&domain)
            .expect("default xmpp domain 'localhost' yields a valid muc domain");
        Self {
            enabled: true,
            domain,
            spaces_jid,
            muc_domain,
            mam_database_url: None,
            inbox_database_url: None,
            spaces_metadata_database_url: None,
            channel_space_links_database_url: None,
            pending_delivery_database_url: None,
            sm_database_url: None,
            pubsub_database_url: None,
            public_websocket_url: Url::parse("ws://localhost:3000/ws")
                .expect("default public XMPP WebSocket URL is valid"),
            native_auth_enabled: true,
            acme: XmppAcmeConfig {
                enabled: false,
                email: None,
                cache_dir: PathBuf::from("certs/acme-cache"),
                production: false,
            },
        }
    }
}

/// Build the conventional `spaces.<domain>` JID for a deployment whose
/// `WADDLE_SPACES_JID` env var is not set.
fn derived_spaces_jid(xmpp_domain: &str) -> anyhow::Result<BareJid> {
    let raw = format!("spaces.{xmpp_domain}");
    raw.parse::<BareJid>().with_context(|| {
        format!("failed to parse derived spaces JID 'spaces.{xmpp_domain}' as BareJid")
    })
}

/// Build the conventional `muc.<domain>` host for a deployment whose
/// `WADDLE_MUC_DOMAIN` env var is not set.
fn derived_muc_domain(xmpp_domain: &str) -> anyhow::Result<DomainPart> {
    let raw = format!("muc.{xmpp_domain}");
    DomainPart::from_str(&raw).with_context(|| {
        format!("failed to parse derived MUC domain 'muc.{xmpp_domain}' as DomainPart")
    })
}

impl XmppConfig {
    /// Load XMPP configuration from environment variables.
    ///
    /// Returns an error when `WADDLE_SPACES_JID` or `WADDLE_MUC_DOMAIN`
    /// is set but cannot be parsed into a typed `BareJid` / `DomainPart`
    /// respectively. Both env vars are optional — when unset the
    /// conventional `spaces.<domain>` / `muc.<domain>` derivations from
    /// `WADDLE_XMPP_DOMAIN` are used.
    pub fn from_env() -> anyhow::Result<Self> {
        let enabled = std::env::var("WADDLE_XMPP_ENABLED")
            .map(|v| v.to_lowercase() != "false" && v != "0")
            .unwrap_or(true);

        let domain =
            std::env::var("WADDLE_XMPP_DOMAIN").unwrap_or_else(|_| "localhost".to_string());

        let spaces_jid = parse_optional_env_value::<BareJid>("WADDLE_SPACES_JID")?
            .map(Ok)
            .unwrap_or_else(|| derived_spaces_jid(&domain))?;
        let muc_domain = parse_optional_env_value::<DomainPart>("WADDLE_MUC_DOMAIN")?
            .map(Ok)
            .unwrap_or_else(|| derived_muc_domain(&domain))?;

        let mam_database_url = resolve_xmpp_database_url("WADDLE_XMPP_MAM_DATABASE_URL");
        let inbox_database_url = resolve_xmpp_database_url("WADDLE_XMPP_INBOX_DATABASE_URL");
        let spaces_metadata_database_url =
            resolve_xmpp_database_url("WADDLE_XMPP_SPACES_METADATA_DATABASE_URL");
        let channel_space_links_database_url =
            resolve_xmpp_database_url("WADDLE_XMPP_CHANNEL_SPACE_LINKS_DATABASE_URL");
        let pending_delivery_database_url =
            resolve_xmpp_database_url("WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL");
        let sm_database_url = resolve_xmpp_database_url("WADDLE_XMPP_SM_DATABASE_URL");
        let pubsub_database_url = resolve_xmpp_database_url("WADDLE_XMPP_PUBSUB_DATABASE_URL");
        let public_websocket_url = parse_public_websocket_url(
            std::env::var("WADDLE_XMPP_PUBLIC_WEBSOCKET_URL")
                .ok()
                .as_deref(),
            &domain,
        )?;

        let native_auth_enabled = std::env::var("WADDLE_NATIVE_AUTH_ENABLED")
            .map(|v| v.to_lowercase() != "false" && v != "0")
            .unwrap_or(true);

        let acme_enabled = std::env::var("WADDLE_XMPP_ACME_ENABLED")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false);
        let acme_email = std::env::var("WADDLE_XMPP_ACME_EMAIL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let acme_cache_dir = std::env::var("WADDLE_XMPP_ACME_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("certs/acme-cache"));
        let acme_production = std::env::var("WADDLE_XMPP_ACME_PRODUCTION")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false);

        Ok(Self {
            enabled,
            domain,
            spaces_jid,
            muc_domain,
            mam_database_url,
            inbox_database_url,
            spaces_metadata_database_url,
            channel_space_links_database_url,
            pending_delivery_database_url,
            sm_database_url,
            pubsub_database_url,
            public_websocket_url,
            native_auth_enabled,
            acme: XmppAcmeConfig {
                enabled: acme_enabled,
                email: acme_email,
                cache_dir: acme_cache_dir,
                production: acme_production,
            },
        })
    }
}

/// Parse the operator-declared public RFC 7395 endpoint. When it is omitted,
/// derive an explicitly plaintext endpoint so security-sensitive features
/// fail closed until the deployment declares its TLS-terminated `wss://`
/// address. Whitespace is normalized here and malformed/HTTP URLs fail
/// startup.
fn parse_public_websocket_url(raw: Option<&str>, xmpp_domain: &str) -> anyhow::Result<Url> {
    let configured = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("ws://{xmpp_domain}/ws"));
    let url = Url::parse(&configured)
        .context("WADDLE_XMPP_PUBLIC_WEBSOCKET_URL is not a valid absolute URL")?;
    if !matches!(url.scheme(), "ws" | "wss") {
        anyhow::bail!("WADDLE_XMPP_PUBLIC_WEBSOCKET_URL must use the ws or wss scheme");
    }
    if url.host().is_none() {
        anyhow::bail!("WADDLE_XMPP_PUBLIC_WEBSOCKET_URL must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("WADDLE_XMPP_PUBLIC_WEBSOCKET_URL must not include user information");
    }
    if url.query().is_some() {
        anyhow::bail!("WADDLE_XMPP_PUBLIC_WEBSOCKET_URL must not include a query");
    }
    if url.fragment().is_some() {
        anyhow::bail!("WADDLE_XMPP_PUBLIC_WEBSOCKET_URL must not include a fragment");
    }
    Ok(url)
}

/// Parse an optional typed env var. Returns `Ok(None)` when the var is
/// unset (or empty after trimming); `Ok(Some(value))` when set and
/// parsed successfully; `Err` with a clear context when set but
/// unparseable.
fn parse_optional_env_value<T>(env_key: &str) -> anyhow::Result<Option<T>>
where
    T: FromStr,
    <T as FromStr>::Err: std::fmt::Display,
{
    let raw = match std::env::var(env_key) {
        Ok(value) => value.trim().to_string(),
        Err(_) => return Ok(None),
    };
    if raw.is_empty() {
        return Ok(None);
    }
    raw.parse::<T>()
        .map(Some)
        .map_err(|error| anyhow::anyhow!("{env_key}='{raw}' failed to parse: {error}"))
        .with_context(|| format!("invalid {env_key} environment variable"))
}

pub(crate) fn resolve_xmpp_database_url(env_key: &str) -> Option<String> {
    std::env::var(env_key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("WADDLE_DATABASE_URL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}
