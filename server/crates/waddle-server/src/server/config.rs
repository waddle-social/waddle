use std::path::PathBuf;

/// XMPP server configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct XmppConfig {
    /// Whether XMPP server is enabled (default: true)
    pub enabled: bool,
    /// XMPP server domain (default: "localhost")
    pub domain: String,
    /// MAM database URL (prefers dedicated XMPP DSN, otherwise the main runtime DSN)
    pub mam_database_url: Option<String>,
    /// Inbox database URL (prefers dedicated XMPP DSN, otherwise the main runtime DSN)
    pub inbox_database_url: Option<String>,
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
        Self {
            enabled: true,
            domain: "localhost".to_string(),
            mam_database_url: None,
            inbox_database_url: None,
            pending_delivery_database_url: None,
            sm_database_url: None,
            pubsub_database_url: None,
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

impl XmppConfig {
    /// Load XMPP configuration from environment variables.
    pub fn from_env() -> Self {
        let enabled = std::env::var("WADDLE_XMPP_ENABLED")
            .map(|v| v.to_lowercase() != "false" && v != "0")
            .unwrap_or(true);

        let domain =
            std::env::var("WADDLE_XMPP_DOMAIN").unwrap_or_else(|_| "localhost".to_string());

        let mam_database_url = resolve_xmpp_database_url("WADDLE_XMPP_MAM_DATABASE_URL");
        let inbox_database_url = resolve_xmpp_database_url("WADDLE_XMPP_INBOX_DATABASE_URL");
        let pending_delivery_database_url =
            resolve_xmpp_database_url("WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL");
        let sm_database_url = resolve_xmpp_database_url("WADDLE_XMPP_SM_DATABASE_URL");
        let pubsub_database_url = resolve_xmpp_database_url("WADDLE_XMPP_PUBSUB_DATABASE_URL");

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

        Self {
            enabled,
            domain,
            mam_database_url,
            inbox_database_url,
            pending_delivery_database_url,
            sm_database_url,
            pubsub_database_url,
            native_auth_enabled,
            acme: XmppAcmeConfig {
                enabled: acme_enabled,
                email: acme_email,
                cache_dir: acme_cache_dir,
                production: acme_production,
            },
        }
    }
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
