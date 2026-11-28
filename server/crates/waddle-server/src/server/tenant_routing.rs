//! Strict `Host`-derived multi-tenant routing and `/etc/waddle/tenants` hot-reload.
//!
//! In multi-tenant mode (`WADDLE_TENANTS_DIR` configured), every application HTTP
//! and WebSocket request resolves its [`ResolvedTenant`] strictly from the HTTP
//! `Host` header (or HTTP/2 `:authority` when `Host` is absent). `X-Forwarded-Host`
//! and caller-claimed query/body/header tenant identifiers are never consulted.
//!
//! Operational endpoints (`/health`, `/healthz`, `/ready`, `/readyz`, `/metrics`,
//! `/api/v1/health`, `/.well-known/acme-challenge/*`) stay mounted on the outer
//! operational router so pod-IP kubelet probes, cluster Service scrapes, and ACME
//! HTTP-01 challenges work without a tenant `Host` header.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use jid::{BareJid, DomainPart};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use url::{Origin, Url};
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::permission::PermissionBackend;
use waddle_xmpp::identity::CanonicalDomain;

use crate::auth::ProviderRegistry;
use crate::db::tenant_registry::ServerTenantRegistry;
use crate::server::routes::websocket::XmppServiceDomains;
use crate::server::tenancy_bootstrap::{
    extract_openfga_store_refs, hash_tenants_dir, load_tenant_manifests_with_digest,
    TenantManifestSpec,
};

/// Default poll interval for `/etc/waddle/tenants` ConfigMap hot-reload.
pub const TENANTS_DIR_RELOAD_INTERVAL: Duration = Duration::from_secs(5);

/// Resolved tenant identity and per-host public URLs attached to an HTTP request
/// or pinned to an XMPP WebSocket session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTenant {
    pub tenant_id: TenantId,
    /// Normalized lowercase DNS hostname matched from the HTTP `Host` header.
    pub canonical_host: String,
    /// Public HTTP(S) base URL for this request's matched hostname (no trailing slash).
    pub public_base_url: Url,
    /// Public WebSocket URL (`wss://<canonical_host>/ws` or `ws://...` in dev).
    pub public_ws_url: Url,
    /// Canonical XMPP account domain for this tenant (`tenant_id`).
    pub xmpp_domain: DomainPart,
    /// Canonical MUC component domain for this tenant (`muc.<xmpp_domain>`).
    pub muc_domain: DomainPart,
    /// Canonical Spaces component bare JID for this tenant (`spaces.<xmpp_domain>`).
    pub spaces_jid: BareJid,
    /// Configured owner localparts for this tenant.
    pub owners: Vec<String>,
    /// Extra browser origins trusted for this tenant (manifest `corsOrigins`).
    pub cors_origins: Vec<Origin>,
}

impl ResolvedTenant {
    /// Construct a [`ResolvedTenant`] for single-tenant fallback or startup default.
    pub fn from_static_defaults(
        tenant_id: TenantId,
        base_url: &str,
        public_ws_url: &Url,
        xmpp_domain: DomainPart,
        muc_domain: DomainPart,
        spaces_jid: BareJid,
        owners: Vec<String>,
    ) -> anyhow::Result<Self> {
        let parsed_base = Url::parse(base_url.trim_end_matches('/'))
            .map_err(|err| anyhow::anyhow!("invalid base_url {base_url:?}: {err}"))?;
        let canonical_host = parsed_base
            .host_str()
            .map(str::to_ascii_lowercase)
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| xmpp_domain.as_str().to_owned());
        Ok(Self {
            tenant_id,
            canonical_host,
            public_base_url: parsed_base,
            public_ws_url: public_ws_url.clone(),
            xmpp_domain,
            muc_domain,
            spaces_jid,
            owners,
            cors_origins: Vec::new(),
        })
    }

    /// Derive XMPP service component domains (`muc`, `spaces`, `upload`, etc.)
    /// for this tenant.
    pub fn service_domains(&self) -> XmppServiceDomains {
        XmppServiceDomains::new(
            self.xmpp_domain.as_str(),
            self.muc_domain.as_str(),
            self.spaces_jid.domain().as_str(),
        )
    }

    /// Public base URL without trailing slash.
    pub fn base_url_str(&self) -> String {
        self.public_base_url
            .as_str()
            .trim_end_matches('/')
            .to_owned()
    }

    /// Per-host OAuth/OIDC callback URL (`<public_base_url>/api/auth/callback`).
    pub fn callback_url(&self) -> String {
        format!("{}/api/auth/callback", self.base_url_str())
    }

    /// Whether the public base URL uses HTTPS (controls `Secure` cookie flag).
    pub fn is_https(&self) -> bool {
        self.public_base_url.scheme() == "https"
    }

    /// Origin of `public_base_url`.
    pub fn origin(&self) -> Origin {
        self.public_base_url.origin()
    }

    /// Returns `true` iff `jid`'s domain matches this tenant's `xmpp_domain`.
    pub fn owns_jid(&self, jid: &BareJid) -> bool {
        jid.domain().as_str() == self.xmpp_domain.as_str()
    }

    /// Returns `true` iff `raw_jid` parses as a bare JID belonging to this tenant.
    pub fn owns_jid_str(&self, raw_jid: &str) -> bool {
        BareJid::from_str(raw_jid).is_ok_and(|jid| self.owns_jid(&jid))
    }
}

/// Errors produced during strict HTTP `Host` extraction and tenant lookup.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TenantHostError {
    #[error("missing Host header")]
    MissingHost,
    #[error("duplicate Host headers")]
    DuplicateHost,
    #[error("malformed Host header")]
    MalformedHost,
    #[error("unknown tenant host: {0}")]
    UnknownHost(String),
    #[error("tenant routing table unavailable")]
    InternalUnavailable,
}

impl TenantHostError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::MissingHost | Self::DuplicateHost | Self::MalformedHost => {
                StatusCode::BAD_REQUEST
            }
            Self::UnknownHost(_) => StatusCode::MISDIRECTED_REQUEST,
            Self::InternalUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    pub fn error_code(&self) -> &'static str {
        match self {
            Self::MissingHost => "missing_host",
            Self::DuplicateHost => "duplicate_host",
            Self::MalformedHost => "malformed_host",
            Self::UnknownHost(_) => "unknown_host",
            Self::InternalUnavailable => "routing_unavailable",
        }
    }
}

impl IntoResponse for TenantHostError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = serde_json::json!({
            "error": self.error_code(),
            "message": self.to_string(),
        });
        (status, axum::Json(body)).into_response()
    }
}

/// Errors building or updating a [`TenantRoutingTable`].
#[derive(Debug, Error)]
pub enum TenantRoutingError {
    #[error("invalid tenant_id '{tenant_id}': {reason}")]
    InvalidTenantId { tenant_id: String, reason: String },
    #[error("invalid hostname '{hostname}' for tenant '{tenant_id}': {reason}")]
    InvalidHostname {
        tenant_id: String,
        hostname: String,
        reason: String,
    },
    #[error("invalid auth providers for tenant '{tenant_id}': {reason}")]
    InvalidAuthProviders { tenant_id: String, reason: String },
    #[error("invalid cors origin for tenant '{tenant_id}': {reason}")]
    InvalidCorsOrigin { tenant_id: String, reason: String },
    #[error(
        "duplicate hostname '{hostname}' claimed by both '{existing_tenant}' and '{conflicting_tenant}'"
    )]
    DuplicateHostname {
        hostname: String,
        existing_tenant: TenantId,
        conflicting_tenant: TenantId,
    },
}

/// Immutable snapshot of host-to-tenant routing entries.
#[derive(Debug, Clone)]
pub struct TenantRoutingTable {
    by_host: HashMap<String, ResolvedTenant>,
    tenants: HashSet<TenantId>,
    allowed_origins: HashSet<Origin>,
    /// Sign-in providers declared by each tenant manifest. Swapped together
    /// with the host map so a request never sees hosts and providers from
    /// different manifest generations.
    auth_providers: HashMap<TenantId, Arc<ProviderRegistry>>,
}

impl TenantRoutingTable {
    pub fn get_by_host(&self, normalized_host: &str) -> Option<&ResolvedTenant> {
        self.by_host.get(normalized_host)
    }

    pub fn is_allowed_origin(&self, origin: &Origin) -> bool {
        self.allowed_origins.contains(origin)
    }

    pub fn tenant_ids(&self) -> &HashSet<TenantId> {
        &self.tenants
    }

    pub fn host_count(&self) -> usize {
        self.by_host.len()
    }

    /// Sign-in providers declared for `tenant_id`, or `None` when its
    /// manifest declares none.
    pub fn auth_providers(&self, tenant_id: &TenantId) -> Option<Arc<ProviderRegistry>> {
        self.auth_providers.get(tenant_id).map(Arc::clone)
    }
}

/// Thread-safe host-to-tenant resolver shared across HTTP handlers, CORS, and
/// WebSocket upgrades.
#[derive(Debug)]
pub struct TenantHostResolver {
    multi_tenant: bool,
    fallback: ResolvedTenant,
    table: RwLock<Arc<TenantRoutingTable>>,
}

impl TenantHostResolver {
    /// Create a single-tenant resolver (dev/test mode when `WADDLE_TENANTS_DIR`
    /// is unset): accepts requests even for localhost/pod-IP hosts by falling
    /// back to `fallback`.
    pub fn single_tenant(fallback: ResolvedTenant) -> Self {
        let mut by_host = HashMap::new();
        let mut tenants = HashSet::new();
        let mut allowed_origins = HashSet::new();
        tenants.insert(fallback.tenant_id.clone());
        allowed_origins.insert(fallback.origin());
        by_host.insert(fallback.canonical_host.clone(), fallback.clone());
        Self {
            multi_tenant: false,
            fallback,
            table: RwLock::new(Arc::new(TenantRoutingTable {
                by_host,
                tenants,
                allowed_origins,
                auth_providers: HashMap::new(),
            })),
        }
    }

    /// Create a strict multi-tenant resolver (`WADDLE_TENANTS_DIR` configured):
    /// unknown or IP-literal `Host` headers fail closed with HTTP 421.
    pub fn multi_tenant(
        fallback: ResolvedTenant,
        manifests: &[TenantManifestSpec],
    ) -> Result<Self, TenantRoutingError> {
        let resolver = Self {
            multi_tenant: true,
            fallback,
            table: RwLock::new(Arc::new(TenantRoutingTable {
                by_host: HashMap::new(),
                tenants: HashSet::new(),
                allowed_origins: HashSet::new(),
                auth_providers: HashMap::new(),
            })),
        };
        let table = resolver.build_table(manifests)?;
        resolver.replace_table(table);
        Ok(resolver)
    }

    pub fn is_multi_tenant(&self) -> bool {
        self.multi_tenant
    }

    pub fn fallback(&self) -> ResolvedTenant {
        self.fallback.clone()
    }

    /// Alias for [`Self::fallback`].
    pub fn fallback_tenant(&self) -> ResolvedTenant {
        self.fallback.clone()
    }

    /// Return the current immutable routing snapshot.
    pub fn snapshot(&self) -> Result<Arc<TenantRoutingTable>, TenantHostError> {
        self.table
            .read()
            .map(|guard| Arc::clone(&guard))
            .map_err(|_| TenantHostError::InternalUnavailable)
    }

    /// Atomically replace the active routing table.
    pub fn replace_table(&self, next: TenantRoutingTable) {
        if let Ok(mut guard) = self.table.write() {
            *guard = Arc::new(next);
        } else {
            error!("TenantHostResolver lock poisoned; refusing to recover poisoned state");
        }
    }

    /// Check whether `origin` matches any currently active tenant host origin.
    pub fn is_allowed_origin(&self, origin: &Origin) -> bool {
        self.snapshot()
            .is_ok_and(|table| table.is_allowed_origin(origin))
    }

    /// Check whether an HTTP `Origin` header value matches any currently active
    /// tenant host origin.
    pub fn is_allowed_origin_str(&self, origin_header: &str) -> bool {
        let Ok(url) = Url::parse(origin_header.trim()) else {
            return false;
        };
        self.is_allowed_origin(&url.origin())
    }

    /// Return the primary public base URL (without trailing slash) for a
    /// tenant's XMPP domain, if registered in the active routing table.
    pub fn base_url_for_domain(&self, xmpp_domain: &str) -> Option<String> {
        let table = self.snapshot().ok()?;
        if let Some(tenant) = table.by_host.get(xmpp_domain) {
            if tenant.xmpp_domain.as_str() == xmpp_domain {
                return Some(tenant.base_url_str());
            }
        }
        table
            .by_host
            .values()
            .find(|tenant| tenant.xmpp_domain.as_str() == xmpp_domain)
            .map(ResolvedTenant::base_url_str)
    }

    /// Returns `true` if `domain` is an active tenant XMPP domain in the
    /// current routing table (updates automatically on `/etc/waddle/tenants`
    /// hot-reload).
    pub fn contains_tenant_domain(&self, domain: &str) -> bool {
        let Ok(table) = self.snapshot() else {
            return false;
        };
        table.tenants.iter().any(|tid| tid.as_str() == domain)
    }

    /// Infer the active [`TenantId`] from a bare JID or component address
    /// (`alice@example.waddle.social`, `room@muc.example.waddle.social`,
    /// `spaces.example.waddle.social`, or `example.waddle.social`).
    ///
    /// Returns `None` if `raw` does not identify a currently active tenant.
    pub fn infer_tenant_id_from_jid(&self, raw: &str) -> Option<TenantId> {
        let table = self.snapshot().ok()?;
        let domain_candidate = if let Ok(bare) = BareJid::from_str(raw) {
            bare.domain().as_str().to_owned()
        } else {
            raw.trim().to_ascii_lowercase()
        };
        if let Some(tid) = table
            .tenants
            .iter()
            .find(|tid| tid.as_str() == domain_candidate)
        {
            return Some(tid.clone());
        }
        for prefix in [
            "muc.",
            "spaces.",
            "upload.",
            "community.",
            "extensions.",
            "push.",
        ] {
            if let Some(stripped) = domain_candidate.strip_prefix(prefix) {
                if let Some(tid) = table.tenants.iter().find(|tid| tid.as_str() == stripped) {
                    return Some(tid.clone());
                }
            }
        }
        None
    }

    /// Resolve a request's [`ResolvedTenant`] strictly from HTTP `Host` (or
    /// HTTP/2 `:authority`). Never inspects `X-Forwarded-Host`.
    pub fn resolve_request(
        &self,
        headers: &HeaderMap,
        uri: &Uri,
    ) -> Result<ResolvedTenant, TenantHostError> {
        let normalized_host = extract_normalized_host(headers, uri, self.multi_tenant)?;
        let table = self.snapshot()?;
        if let Some(resolved) = table.get_by_host(&normalized_host) {
            return Ok(resolved.clone());
        }
        if self.multi_tenant {
            return Err(TenantHostError::UnknownHost(normalized_host));
        }
        Ok(self.fallback.clone())
    }

    /// Build and validate a new [`TenantRoutingTable`] from loaded manifests.
    ///
    /// Rejects duplicate hostname claims across distinct tenants so hot-reload
    /// can fail safe and retain the prior routing table.
    pub fn build_table(
        &self,
        manifests: &[TenantManifestSpec],
    ) -> Result<TenantRoutingTable, TenantRoutingError> {
        let mut by_host: HashMap<String, ResolvedTenant> = HashMap::new();
        let mut tenants: HashSet<TenantId> = HashSet::new();
        let mut allowed_origins: HashSet<Origin> = HashSet::new();
        let mut auth_providers: HashMap<TenantId, Arc<ProviderRegistry>> = HashMap::new();

        for spec in manifests {
            let tenant_id = TenantId::parse(&spec.tenant_id).map_err(|err| {
                TenantRoutingError::InvalidTenantId {
                    tenant_id: spec.tenant_id.clone(),
                    reason: err.to_string(),
                }
            })?;
            let xmpp_domain = DomainPart::from_str(tenant_id.as_str()).map_err(|err| {
                TenantRoutingError::InvalidTenantId {
                    tenant_id: spec.tenant_id.clone(),
                    reason: err.to_string(),
                }
            })?;
            let (muc_domain, spaces_jid) =
                if tenant_id == self.fallback.tenant_id {
                    (
                        self.fallback.muc_domain.clone(),
                        self.fallback.spaces_jid.clone(),
                    )
                } else {
                    let muc = DomainPart::from_str(&format!("muc.{}", tenant_id.as_str()))
                        .map_err(|err| TenantRoutingError::InvalidTenantId {
                            tenant_id: spec.tenant_id.clone(),
                            reason: format!("invalid derived muc domain: {err}"),
                        })?;
                    let spaces = BareJid::from_str(&format!("spaces.{}", tenant_id.as_str()))
                        .map_err(|err| TenantRoutingError::InvalidTenantId {
                            tenant_id: spec.tenant_id.clone(),
                            reason: format!("invalid derived spaces JID: {err}"),
                        })?;
                    (muc, spaces)
                };

            let raw_hosts: Vec<&str> = if spec.hostnames.is_empty() {
                vec![tenant_id.as_str()]
            } else {
                spec.hostnames.iter().map(String::as_str).collect()
            };

            tenants.insert(tenant_id.clone());

            let mut cors_origins: Vec<Origin> = Vec::new();
            for entry in &spec.cors_origins {
                let origin = crate::auth::redirect::parse_trusted_origin(entry.trim()).map_err(
                    |err| TenantRoutingError::InvalidCorsOrigin {
                        tenant_id: spec.tenant_id.clone(),
                        reason: err.to_string(),
                    },
                )?;
                if !cors_origins.contains(&origin) {
                    cors_origins.push(origin);
                }
            }
            allowed_origins.extend(cors_origins.iter().cloned());

            if !spec.auth_providers.is_empty() {
                let registry = ProviderRegistry::try_from_tenant_specs(spec.auth_providers.clone())
                    .map_err(|err| TenantRoutingError::InvalidAuthProviders {
                        tenant_id: spec.tenant_id.clone(),
                        reason: err.to_string(),
                    })?;
                auth_providers.insert(tenant_id.clone(), Arc::new(registry));
            }

            for raw_host in raw_hosts {
                let canonical_host = normalize_manifest_hostname(raw_host).map_err(|reason| {
                    TenantRoutingError::InvalidHostname {
                        tenant_id: spec.tenant_id.clone(),
                        hostname: raw_host.to_owned(),
                        reason,
                    }
                })?;
                if let Some(existing) = by_host.get(&canonical_host) {
                    if existing.tenant_id != tenant_id {
                        return Err(TenantRoutingError::DuplicateHostname {
                            hostname: canonical_host,
                            existing_tenant: existing.tenant_id.clone(),
                            conflicting_tenant: tenant_id,
                        });
                    }
                    continue;
                }

                let (public_base_url, public_ws_url) =
                    build_tenant_urls(&canonical_host, &self.fallback);
                allowed_origins.insert(public_base_url.origin());
                by_host.insert(
                    canonical_host.clone(),
                    ResolvedTenant {
                        tenant_id: tenant_id.clone(),
                        canonical_host,
                        public_base_url,
                        public_ws_url,
                        xmpp_domain: xmpp_domain.clone(),
                        muc_domain: muc_domain.clone(),
                        spaces_jid: spaces_jid.clone(),
                        owners: spec.owners.clone(),
                        cors_origins: cors_origins.clone(),
                    },
                );
            }
        }

        // Always keep the configured default tenant's fallback host (e.g.
        // `next.apps.waddle.chat`) routable if no other tenant claimed it.
        tenants.insert(self.fallback.tenant_id.clone());
        if !by_host.contains_key(&self.fallback.canonical_host) {
            allowed_origins.insert(self.fallback.origin());
            by_host.insert(self.fallback.canonical_host.clone(), self.fallback.clone());
        }

        Ok(TenantRoutingTable {
            by_host,
            tenants,
            allowed_origins,
            auth_providers,
        })
    }
}

fn build_tenant_urls(canonical_host: &str, fallback: &ResolvedTenant) -> (Url, Url) {
    let use_insecure =
        canonical_host == fallback.canonical_host && fallback.public_base_url.scheme() == "http";
    let http_scheme = if use_insecure { "http" } else { "https" };
    let ws_scheme = if use_insecure { "ws" } else { "wss" };

    let base_str = if canonical_host == fallback.canonical_host {
        fallback.base_url_str()
    } else {
        format!("{http_scheme}://{canonical_host}")
    };
    let ws_str = if canonical_host == fallback.canonical_host && use_insecure {
        fallback.public_ws_url.as_str().to_owned()
    } else {
        format!("{ws_scheme}://{canonical_host}/ws")
    };

    let base = Url::parse(&base_str).unwrap_or_else(|_| fallback.public_base_url.clone());
    let ws = Url::parse(&ws_str).unwrap_or_else(|_| fallback.public_ws_url.clone());
    (base, ws)
}

fn normalize_manifest_hostname(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("hostname must not be empty".to_owned());
    }
    if trimmed.contains('*') || trimmed.ends_with('.') || trimmed.contains(':') {
        return Err(
            "hostname must be a plain FQDN without wildcards, port, or trailing dot".to_owned(),
        );
    }
    if trimmed.parse::<IpAddr>().is_ok() {
        return Err("IP literals are not valid tenant hostnames".to_owned());
    }
    let lowered = trimmed.to_ascii_lowercase();
    if lowered != trimmed {
        return Err("hostname must be lowercase".to_owned());
    }
    CanonicalDomain::parse_canonical(&lowered)
        .map_err(|err| format!("not a canonical DNS domain: {err}"))?;
    Ok(lowered)
}

/// Strictly extract and normalize the request hostname from the HTTP `Host`
/// header (or HTTP/2 `:authority` if no `Host` header is present).
///
/// - Never consults `X-Forwarded-Host` or other proxy headers.
/// - Rejects duplicate `Host` headers (`DuplicateHost` -> HTTP 400).
/// - Rejects missing or malformed `Host` headers (`MissingHost` / `MalformedHost` -> HTTP 400).
/// - In `multi_tenant` mode: strips only standard `:80`/`:443` ports, rejects
///   non-standard ports and IP literals (`UnknownHost` -> HTTP 421).
pub fn extract_normalized_host(
    headers: &HeaderMap,
    uri: &Uri,
    multi_tenant: bool,
) -> Result<String, TenantHostError> {
    let mut host_values = headers.get_all(header::HOST).iter();
    let raw_authority = match (host_values.next(), host_values.next()) {
        (Some(_), Some(_)) => return Err(TenantHostError::DuplicateHost),
        (Some(value), None) => value.to_str().map_err(|_| TenantHostError::MalformedHost)?,
        (None, _) => match uri.authority() {
            Some(authority) => authority.as_str(),
            None => {
                if multi_tenant {
                    return Err(TenantHostError::MissingHost);
                }
                return Ok(String::new());
            }
        },
    };

    if raw_authority.is_empty()
        || raw_authority.chars().any(char::is_whitespace)
        || raw_authority.contains('@')
        || raw_authority.contains('/')
        || raw_authority.contains('?')
        || raw_authority.contains('#')
        || raw_authority.contains('*')
    {
        return Err(TenantHostError::MalformedHost);
    }

    // Handle IPv6 bracketed literals (`[::1]` or `[::1]:443`).
    if raw_authority.starts_with('[') {
        let Some(end_bracket) = raw_authority.find(']') else {
            return Err(TenantHostError::MalformedHost);
        };
        let ip_inner = &raw_authority[1..end_bracket];
        if ip_inner.parse::<IpAddr>().is_err() {
            return Err(TenantHostError::MalformedHost);
        }
        let rest = &raw_authority[end_bracket + 1..];
        if !rest.is_empty() {
            let Some(port_str) = rest.strip_prefix(':') else {
                return Err(TenantHostError::MalformedHost);
            };
            if port_str.parse::<u16>().is_err() {
                return Err(TenantHostError::MalformedHost);
            }
        }
        if multi_tenant {
            return Err(TenantHostError::UnknownHost(
                raw_authority.to_ascii_lowercase(),
            ));
        }
        return Ok(raw_authority.to_ascii_lowercase());
    }

    let (host_part, port_opt) = match raw_authority.rsplit_once(':') {
        Some((h, p)) => {
            if h.contains(':') {
                // Unbracketed multi-colon authority is malformed.
                return Err(TenantHostError::MalformedHost);
            }
            let port = p
                .parse::<u16>()
                .map_err(|_| TenantHostError::MalformedHost)?;
            (h, Some(port))
        }
        None => (raw_authority, None),
    };

    if host_part.is_empty() || host_part.ends_with('.') || host_part.starts_with('.') {
        return Err(TenantHostError::MalformedHost);
    }

    let lowered = host_part.to_ascii_lowercase();
    if lowered.parse::<IpAddr>().is_ok() {
        if multi_tenant {
            return Err(TenantHostError::UnknownHost(lowered));
        }
        return Ok(lowered);
    }

    if multi_tenant {
        if let Some(port) = port_opt {
            if port != 80 && port != 443 {
                return Err(TenantHostError::UnknownHost(format!("{lowered}:{port}")));
            }
        }
        CanonicalDomain::parse_canonical(&lowered).map_err(|_| TenantHostError::MalformedHost)?;
    }

    Ok(lowered)
}

/// Axum middleware that strictly resolves [`ResolvedTenant`] from the HTTP
/// `Host` header and attaches it to request extensions.
pub async fn resolve_tenant_host(
    State(resolver): State<Arc<TenantHostResolver>>,
    mut request: Request,
    next: Next,
) -> Response {
    match resolver.resolve_request(request.headers(), request.uri()) {
        Ok(resolved) => {
            request.extensions_mut().insert(resolved);
            next.run(request).await
        }
        Err(err) => {
            warn!(error = %err, path = %request.uri().path(), "rejected request with invalid or unknown Host");
            err.into_response()
        }
    }
}

/// Outcome of a single `/etc/waddle/tenants` hot-reload pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantsReloadOutcome {
    Unchanged,
    Reloaded {
        host_count: usize,
        tenant_count: usize,
    },
}

/// Perform a single fail-safe hot-reload pass of `tenants_dir`.
///
/// Ordering invariant:
/// 1. Parse manifests and validate the new [`TenantRoutingTable`] (rejecting
///    parse errors or duplicate host claims without disturbing the active table).
/// 2. Register OpenFGA store refs and call `tenant_registry.ensure_tenant` for
///    every tenant in the new manifests **before** publishing the new table.
/// 3. Atomically swap the new routing table into `resolver`.
/// 4. Evict (never purge) tenants removed from the prior table.
pub async fn reconcile_tenants_dir_once(
    tenants_dir: &Path,
    last_digest: &mut Option<[u8; 32]>,
    resolver: &TenantHostResolver,
    tenant_registry: Option<&ServerTenantRegistry>,
    permission_backend: Option<&Arc<dyn PermissionBackend>>,
) -> anyhow::Result<TenantsReloadOutcome> {
    let current_digest = hash_tenants_dir(tenants_dir).await?;
    if current_digest == *last_digest {
        return Ok(TenantsReloadOutcome::Unchanged);
    }

    let Some((digest, manifests)) = load_tenant_manifests_with_digest(tenants_dir).await? else {
        *last_digest = None;
        return Ok(TenantsReloadOutcome::Unchanged);
    };

    let next_table = resolver
        .build_table(&manifests)
        .map_err(|err| anyhow::anyhow!("rejected invalid tenant routing update: {err}"))?;

    // Step 1: Register OpenFGA store refs before provisioning/swapping.
    let store_refs = extract_openfga_store_refs(&manifests)?;
    if let Some(backend) = permission_backend {
        for store_ref in store_refs {
            backend
                .register_tenant_store_ref(store_ref)
                .map_err(|err| {
                    anyhow::anyhow!("failed to register OpenFGA store ref during reload: {err}")
                })?;
        }
    }

    // Step 2: Ensure Turso DB + OpenFGA binding for every declared tenant BEFORE
    // publishing the new routing table.
    if let Some(registry) = tenant_registry {
        for tenant_id in next_table.tenant_ids() {
            registry.ensure_tenant(tenant_id).await.map_err(|err| {
                anyhow::anyhow!(
                    "failed to ensure Turso/OpenFGA store for tenant '{tenant_id}' during reload: {err}"
                )
            })?;
        }
    }

    let previous_tenants = resolver
        .snapshot()
        .map(|snap| snap.tenant_ids().clone())
        .unwrap_or_default();
    let next_tenants = next_table.tenant_ids().clone();
    let host_count = next_table.host_count();
    let tenant_count = next_tenants.len();

    // Step 3: Atomically swap active routing table.
    resolver.replace_table(next_table);
    *last_digest = Some(digest);

    // Step 4: Evict (never purge) removed tenants.
    if let Some(registry) = tenant_registry {
        let default_tenant = resolver.fallback().tenant_id;
        for removed in previous_tenants.difference(&next_tenants) {
            if removed == &default_tenant {
                continue;
            }
            if let Err(err) = registry.evict_tenant(removed).await {
                warn!(tenant_id = %removed, %err, "failed to evict removed tenant during hot-reload");
            }
        }
    }

    info!(
        host_count,
        tenant_count,
        dir = %tenants_dir.display(),
        "Hot-reloaded tenant routing table from WADDLE_TENANTS_DIR"
    );
    Ok(TenantsReloadOutcome::Reloaded {
        host_count,
        tenant_count,
    })
}

/// Spawn the background 5s `/etc/waddle/tenants` hot-reloader task.
pub fn spawn_tenants_dir_reloader(
    tenants_dir: PathBuf,
    initial_digest: Option<[u8; 32]>,
    resolver: Arc<TenantHostResolver>,
    tenant_registry: Option<Arc<ServerTenantRegistry>>,
    permission_backend: Option<Arc<dyn PermissionBackend>>,
    stop_token: CancellationToken,
) {
    tokio::spawn(async move {
        let mut last_digest = initial_digest;
        let mut ticker = tokio::time::interval(TENANTS_DIR_RELOAD_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Consume the immediate first tick so startup does not re-scan at t=0.
        ticker.tick().await;
        loop {
            tokio::select! {
                () = stop_token.cancelled() => {
                    info!("Stopping WADDLE_TENANTS_DIR hot-reloader");
                    break;
                }
                _ = ticker.tick() => {
                    if let Err(err) = reconcile_tenants_dir_once(
                        &tenants_dir,
                        &mut last_digest,
                        &resolver,
                        tenant_registry.as_deref(),
                        permission_backend.as_ref(),
                    )
                    .await
                    {
                        warn!(%err, dir = %tenants_dir.display(), "WADDLE_TENANTS_DIR hot-reload failed; retaining previous routing table");
                    }
                }
            }
        }
    });
}
