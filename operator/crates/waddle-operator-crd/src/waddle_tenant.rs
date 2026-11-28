use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::dependencies::LocalObjectRef;
use crate::identifiers::{ResourceName, XmppDomain};
use crate::server_config::IngressSpec;
use crate::waddle_server::conditions_schema;

/// Always-enforced built-in reserved single-label subdomains under `managedBaseDomain`.
pub const BUILTIN_RESERVED_SUBDOMAINS: &[&str] = &[
    "xmpp",
    "sfu",
    "turn",
    "teleport",
    "www",
    "api",
    "admin",
    "auth",
    "colony",
    "mail",
    "ns1",
    "ns2",
    "status",
    "docs",
    "apps",
    "next",
    "muc",
    "spaces",
    "upload",
    "extensions",
    "push",
    "community",
    "chat",
];

/// Ingress routing mode for a `WaddleTenant`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TenantIngressMode {
    /// Automatically select `ManagedSubdomain` when hostnames sit under the server's
    /// `managedBaseDomain`, or `CustomDomain` otherwise.
    #[default]
    Auto,
    /// Route under the server's managed wildcard domain (`<slug>.<managedBaseDomain>`).
    ManagedSubdomain,
    /// Route a customer-owned domain with per-tenant `Certificate`, `ReferenceGrant`, and
    /// `Gateway` listener.
    CustomDomain,
    /// Do not manage Gateway, DNS, or TLS resources (hostnames are still projected to the server).
    Disabled,
}

/// Domain routing and ingress configuration for a `WaddleTenant`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TenantIngressSpec {
    /// Ingress reconciliation mode (`Auto`, `ManagedSubdomain`, `CustomDomain`, or `Disabled`).
    #[serde(default)]
    pub mode: TenantIngressMode,
    /// Explicit lower-case FQDNs served for this tenant (defaults to `[spec.tenantId]` when empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 16), inner(length(max = 253)))]
    #[x_kube(
        validation = r#"self.all(h, size(h) > 0 && h.matches('^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$'))"#
    )]
    pub hostnames: Vec<String>,
}

/// Concrete ingress routing mode resolved after evaluating tenant and server ingress configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedIngressMode {
    /// No Gateway, DNS, or TLS resources are managed by the operator for this tenant.
    Disabled,
    /// Managed wildcard subdomain routing (`HTTPRoute`s + `DNSEndpoint`, zero `Gateway` mutation).
    ManagedSubdomain,
    /// Bring-your-own custom domain routing (`Certificate`, `ReferenceGrant`, per-tenant `Gateway` SSA, `HTTPRoute`s).
    CustomDomain,
}

/// Validated ingress plan derived from [`WaddleTenantSpec`] and the hosting server's [`IngressSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTenantIngress {
    /// Effective ingress mode to reconcile.
    pub mode: ResolvedIngressMode,
    /// Canonical hostnames projected for this tenant.
    pub hostnames: Vec<String>,
}

/// Validation failure when resolving tenant ingress against the hosting `WaddleServer`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TenantIngressError {
    /// Hostname is not a valid lower-case FQDN (e.g. IP literal, wildcard, uppercase, or single label).
    #[error("ingress hostname '{hostname}' must be a valid lower-case FQDN (no wildcards, ports, or IP literals)")]
    InvalidHostname {
        /// Offending hostname.
        hostname: String,
    },
    /// Server ingress does not configure `managedBaseDomain` while tenant requested `ManagedSubdomain`.
    #[error(
        "spec.ingress.mode=ManagedSubdomain requires WaddleServer spec.ingress.managedBaseDomain"
    )]
    ManagedBaseDomainNotConfigured,
    /// Tenant attempted to claim the apex `managedBaseDomain` (e.g. `waddle.social`) for managed/BYO routing.
    #[error("hostname '{hostname}' matches apex managedBaseDomain '{base_domain}'; set spec.ingress.mode=Disabled for static apex routing")]
    ApexManagedBaseDomainForbidden {
        /// Offending hostname.
        hostname: String,
        /// Server `managedBaseDomain`.
        base_domain: String,
    },
    /// Tenant requested a multi-label subdomain beneath `managedBaseDomain` (e.g. `a.b.waddle.social`).
    #[error(
        "hostname '{hostname}' under '{base_domain}' must be a single label (<slug>.{base_domain})"
    )]
    MultiLabelManagedSubdomain {
        /// Offending hostname.
        hostname: String,
        /// Server `managedBaseDomain`.
        base_domain: String,
    },
    /// Subdomain label is reserved by built-in platform rules or `WaddleServer.spec.ingress.reservedSubdomains`.
    #[error("subdomain '{subdomain}' under '{base_domain}' is reserved")]
    ReservedSubdomain {
        /// Reserved single-label subdomain.
        subdomain: String,
        /// Server `managedBaseDomain`.
        base_domain: String,
    },
    /// `ManagedSubdomain` mode received a hostname outside `managedBaseDomain`.
    #[error("hostname '{hostname}' is not a subdomain of managedBaseDomain '{base_domain}'")]
    HostnameNotUnderManagedBaseDomain {
        /// Offending hostname.
        hostname: String,
        /// Server `managedBaseDomain`.
        base_domain: String,
    },
    /// `CustomDomain` mode received a hostname under `managedBaseDomain`.
    #[error("hostname '{hostname}' is under managedBaseDomain '{base_domain}'; use ManagedSubdomain mode")]
    CustomDomainUnderManagedBaseDomain {
        /// Offending hostname.
        hostname: String,
        /// Server `managedBaseDomain`.
        base_domain: String,
    },
    /// `Auto` mode received a mix of managed subdomains and custom domains on the same tenant.
    #[error("spec.ingress.mode=Auto cannot mix managed subdomains under '{base_domain}' with custom domains")]
    MixedIngressHostnameModes {
        /// Server `managedBaseDomain`.
        base_domain: String,
    },
}

/// Placement mode for a `WaddleTenant` across Waddle server cells.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TenantPlacementMode {
    /// Tenant runs on a shared multi-tenant cell pool.
    #[default]
    SharedPool,
    /// Tenant is pinned to a dedicated single-tenant cell.
    DedicatedCell,
}

/// Optional resource and spend quotas enforced for a `WaddleTenant`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TenantQuotasSpec {
    /// Maximum concurrent XMPP/WebSocket sessions for this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_connections: Option<u32>,
    /// Maximum rooms/channels allowed in this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rooms: Option<u32>,
    /// Maximum aggregate tenant storage in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_storage_bytes: Option<u64>,
    /// Maximum inference spend budget in microcents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_inference_spend_microcents: Option<u64>,
}

/// Extension and inference egress policy settings for a `WaddleTenant`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TenantExtensionsSpec {
    /// Extension plugin identifiers enabled for this tenant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_plugins: Vec<String>,
    /// Inference providers approved by tenant policy (e.g. `openrouter`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_providers: Vec<String>,
    /// Model identifiers approved by tenant policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_models: Vec<String>,
    /// Require Zero Data Retention (ZDR) endpoints for inference requests.
    #[serde(default)]
    pub zdr_required: bool,
}

/// Declarative specification for a `WaddleTenant` resource.
#[derive(CustomResource, KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[kube(
    group = "waddle.social",
    version = "v1alpha1",
    kind = "WaddleTenant",
    plural = "waddletenants",
    namespaced,
    doc = "Declarative multi-tenant configuration and OpenFGA store lifecycle for a WaddleServer.",
    status = "WaddleTenantStatus",
    derive = "PartialEq",
    cel
)]
#[kube(shortname = "wt")]
#[kube(
    printcolumn = r#"{"name":"Tenant","type":"string","description":"Canonical tenant DNS domain","jsonPath":".spec.tenantId"}"#
)]
#[kube(
    printcolumn = r#"{"name":"Server","type":"string","description":"Referenced WaddleServer name","jsonPath":".spec.serverRef.name"}"#
)]
#[kube(
    printcolumn = r#"{"name":"Mode","type":"string","description":"Tenant placement mode","jsonPath":".spec.placementMode"}"#
)]
#[kube(
    printcolumn = r#"{"name":"Phase","type":"string","description":"Current tenant lifecycle phase","jsonPath":".status.phase"}"#
)]
#[kube(
    printcolumn = r#"{"name":"StoreID","type":"string","description":"Provisioned OpenFGA store ID","jsonPath":".status.openfgaStoreId"}"#
)]
#[kube(printcolumn(
    name = "Age",
    type_ = "date",
    json_path = ".metadata.creationTimestamp"
))]
#[serde(rename_all = "camelCase")]
pub struct WaddleTenantSpec {
    /// Reference to the `WaddleServer` hosting this tenant in the same namespace.
    pub server_ref: LocalObjectRef,

    /// Canonical lower-case DNS domain identifying the tenant (e.g. `waddle.social`).
    #[schemars(length(max = 253))]
    #[x_kube(
        validation = r#"size(self) > 0 && self.matches('^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$')"#
    )]
    pub tenant_id: String,

    /// Tenant placement mode (`SharedPool` or `DedicatedCell`).
    #[serde(default)]
    pub placement_mode: TenantPlacementMode,

    /// Localparts granted initial tenant owner privileges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<String>,

    /// Sign-in providers for this tenant. A tenant without providers has
    /// sign-in disabled; it never inherits server-wide providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<TenantAuthSpec>,

    /// Optional per-tenant connection, room, storage, and inference spend quotas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotas: Option<TenantQuotasSpec>,

    /// Optional per-tenant extension and inference provider allowlists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<TenantExtensionsSpec>,

    /// Optional per-tenant domain routing and Gateway API / TLS ingress configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress: Option<TenantIngressSpec>,
}

impl WaddleTenantSpec {
    /// Returns the effective ingress mode (`Auto` when `spec.ingress` is omitted).
    #[must_use]
    pub fn effective_ingress_mode(&self) -> TenantIngressMode {
        self.ingress
            .as_ref()
            .map_or(TenantIngressMode::Auto, |i| i.mode)
    }

    /// Returns the effective hostnames routed to this tenant (`[spec.tenantId]` when
    /// `spec.ingress.hostnames` is omitted or empty).
    #[must_use]
    pub fn effective_hostnames(&self) -> Vec<String> {
        if let Some(ingress) = &self.ingress {
            if !ingress.hostnames.is_empty() {
                return ingress.hostnames.clone();
            }
        }
        vec![self.tenant_id.clone()]
    }
}

/// Sign-in configuration for a `WaddleTenant`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TenantAuthSpec {
    /// OIDC or OAuth2 providers users of this tenant can sign in with.
    #[serde(default)]
    #[schemars(length(max = 8))]
    pub providers: Vec<TenantAuthProvider>,
}

/// Protocol spoken by a tenant sign-in provider.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantAuthProviderKind {
    /// OpenID Connect (discovery, ID token).
    #[serde(rename = "oidc")]
    Oidc,
    /// Plain OAuth2 (token + userinfo endpoints).
    #[serde(rename = "oauth2")]
    OAuth2,
}

/// A public (PKCE) sign-in client for a tenant. Client secrets are not
/// supported: a static client authenticates with PKCE only, and a dynamically
/// registered client uses the method its provider assigns.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TenantAuthProvider {
    /// Provider identifier, unique within the tenant (used in sign-in URLs).
    #[schemars(length(min = 1, max = 64))]
    pub id: String,
    /// Name shown on the sign-in button.
    #[schemars(length(min = 1, max = 128))]
    pub display_name: String,
    /// Provider protocol.
    pub kind: TenantAuthProviderKind,
    /// Register a client with the provider at runtime (RFC 7591) instead of
    /// using `clientId`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dynamic_client_registration: bool,
    /// Pre-registered public client ID (required unless
    /// `dynamicClientRegistration` is set).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[schemars(length(max = 256))]
    pub client_id: String,
    /// Bind tokens with DPoP.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_dpop: bool,
    /// Requested scopes (OIDC providers must include `openid`).
    #[schemars(length(min = 1, max = 16), inner(length(max = 128)))]
    pub scopes: Vec<String>,
    /// HTTPS issuer URL (OIDC discovery base).
    #[schemars(length(max = 512))]
    pub issuer: String,
    /// Authorization endpoint override (required for `oauth2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 512))]
    pub authorization_endpoint: Option<String>,
    /// Token endpoint override (required for `oauth2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 512))]
    pub token_endpoint: Option<String>,
    /// Userinfo endpoint override (required for `oauth2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 512))]
    pub userinfo_endpoint: Option<String>,
    /// JWKS URI override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 512))]
    pub jwks_uri: Option<String>,
    /// Claim holding the stable subject identifier (default `sub`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 128))]
    pub subject_claim: Option<String>,
    /// Claim used as the username / XMPP localpart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 128))]
    pub username_claim: Option<String>,
    /// Claim holding the email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 128))]
    pub email_claim: Option<String>,
}

impl TenantAuthProvider {
    fn is_valid(&self) -> bool {
        let non_empty = |v: &str| !v.trim().is_empty();
        let https = |v: &str| v.starts_with("https://") && v.len() > "https://".len();
        let optional_https = |v: &Option<String>| v.as_deref().is_none_or(https);
        let kind_ok = match self.kind {
            TenantAuthProviderKind::Oidc => self.scopes.iter().any(|s| s == "openid"),
            TenantAuthProviderKind::OAuth2 => {
                self.authorization_endpoint.is_some()
                    && self.token_endpoint.is_some()
                    && self.userinfo_endpoint.is_some()
            }
        };
        non_empty(&self.id)
            && non_empty(&self.display_name)
            && (self.dynamic_client_registration || non_empty(&self.client_id))
            && !self.scopes.is_empty()
            && self.scopes.iter().all(|s| non_empty(s))
            && https(&self.issuer)
            && optional_https(&self.authorization_endpoint)
            && optional_https(&self.token_endpoint)
            && optional_https(&self.userinfo_endpoint)
            && optional_https(&self.jwks_uri)
            && kind_ok
    }
}

/// Lifecycle phase reported in `WaddleTenant.status.phase`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaddleTenantPhase {
    /// Waiting for referenced `WaddleServer` to become `Ready`.
    #[default]
    Pending,
    /// Provisioning OpenFGA store/model and tenant projection ConfigMap.
    Provisioning,
    /// Tenant OpenFGA store/model and projection ConfigMap are reconciled and ready.
    Ready,
    /// Tenant spec invalid or OpenFGA/projection reconciliation encountered an error.
    Degraded,
    /// Tenant has a deletion timestamp and cleanup finalizer is running.
    Terminating,
}

impl WaddleTenantPhase {
    /// Returns the Kubernetes status phase string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Provisioning => "Provisioning",
            Self::Ready => "Ready",
            Self::Degraded => "Degraded",
            Self::Terminating => "Terminating",
        }
    }
}

/// Observed status subresource for `WaddleTenant`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WaddleTenantStatus {
    /// Current tenant lifecycle phase.
    pub phase: WaddleTenantPhase,
    /// Latest `.metadata.generation` processed by the controller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Provisioned OpenFGA store ID for this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openfga_store_id: Option<String>,
    /// Provisioned immutable OpenFGA authorization model ID for this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openfga_model_id: Option<String>,
    /// Current placement lease epoch assigned to this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_epoch: Option<u64>,
    /// Effective hostnames projected and routed for this tenant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hostnames: Vec<String>,
    /// Standard Kubernetes status conditions keyed by `type`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "conditions_schema")]
    pub conditions: Vec<Condition>,
}

/// Individual validation rule failure for [`WaddleTenantSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TenantValidationViolation {
    /// `serverRef.name` is not a valid Kubernetes resource name.
    InvalidServerRefName,
    /// `tenantId` is not a valid canonical lower-case DNS domain.
    InvalidTenantId,
    /// An `auth.providers` entry is incomplete or uses a non-HTTPS URL.
    InvalidAuthProvider,
    /// `auth.providers` contains duplicate provider ids.
    DuplicateAuthProviderId,
    /// One or more `owners` entries were empty or whitespace-only.
    EmptyOwnerEntry,
    /// One or more `ingress.hostnames` entries failed canonical lower-case FQDN validation.
    InvalidIngressHostname,
    /// `ingress.hostnames` contains duplicate entries.
    DuplicateIngressHostname,
}

impl fmt::Display for TenantValidationViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidServerRefName => {
                write!(
                    f,
                    "spec.serverRef.name must be a valid lower-case Kubernetes resource name"
                )
            }
            Self::InvalidTenantId => {
                write!(
                    f,
                    "spec.tenantId must be a valid canonical lower-case DNS domain"
                )
            }
            Self::InvalidAuthProvider => write!(
                f,
                "spec.auth.providers entries need an id, displayName, scopes (openid for oidc), \
                 an HTTPS issuer, a clientId unless dynamicClientRegistration is set, and the \
                 authorization/token/userinfo endpoints for oauth2"
            ),
            Self::DuplicateAuthProviderId => {
                write!(f, "spec.auth.providers ids must be unique")
            }
            Self::EmptyOwnerEntry => {
                write!(
                    f,
                    "spec.owners entries must not be empty or whitespace-only"
                )
            }
            Self::InvalidIngressHostname => {
                write!(
                    f,
                    "spec.ingress.hostnames entries must be valid lower-case RFC-1123 FQDNs"
                )
            }
            Self::DuplicateIngressHostname => {
                write!(
                    f,
                    "spec.ingress.hostnames must not contain duplicate entries"
                )
            }
        }
    }
}

/// Aggregated validation errors for a [`WaddleTenantSpec`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid WaddleTenant specification ({} violation(s))", .violations.len())]
pub struct TenantValidationReport {
    violations: Vec<TenantValidationViolation>,
}

impl TenantValidationReport {
    /// Returns the ordered list of tenant spec validation violations.
    #[must_use]
    pub fn violations(&self) -> &[TenantValidationViolation] {
        &self.violations
    }

    /// Renders concise, semicolon-delimited diagnostic messages safe for CRD status/events.
    #[must_use]
    pub fn summary(&self) -> String {
        self.violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Validates a [`WaddleTenantSpec`] against canonical naming and reference invariants.
///
/// # Errors
/// Returns [`TenantValidationReport`] when one or more invariants fail.
pub fn validate_tenant_spec(spec: &WaddleTenantSpec) -> Result<XmppDomain, TenantValidationReport> {
    let mut violations = Vec::new();

    if ResourceName::try_new(&spec.server_ref.name).is_err() {
        violations.push(TenantValidationViolation::InvalidServerRefName);
    }

    let domain = match XmppDomain::try_new(spec.tenant_id.trim()) {
        Ok(d) => Some(d),
        Err(_) => {
            violations.push(TenantValidationViolation::InvalidTenantId);
            None
        }
    };

    if let Some(auth) = &spec.auth {
        if auth.providers.iter().any(|p| !p.is_valid()) {
            violations.push(TenantValidationViolation::InvalidAuthProvider);
        }
        let mut ids = BTreeSet::new();
        if !auth.providers.iter().all(|p| ids.insert(p.id.as_str())) {
            violations.push(TenantValidationViolation::DuplicateAuthProviderId);
        }
    }

    if spec.owners.iter().any(|owner| owner.trim().is_empty()) {
        violations.push(TenantValidationViolation::EmptyOwnerEntry);
    }

    if let Some(ingress) = &spec.ingress {
        let mut seen = BTreeSet::new();
        let mut has_invalid = false;
        let mut has_dup = false;
        for host in &ingress.hostnames {
            if host != host.trim()
                || XmppDomain::try_new(host).is_err()
                || host.parse::<IpAddr>().is_ok()
            {
                has_invalid = true;
            }
            if !seen.insert(host.as_str()) {
                has_dup = true;
            }
        }
        if has_invalid {
            violations.push(TenantValidationViolation::InvalidIngressHostname);
        }
        if has_dup {
            violations.push(TenantValidationViolation::DuplicateIngressHostname);
        }
    }

    if !violations.is_empty() {
        return Err(TenantValidationReport { violations });
    }

    Ok(domain
        .unwrap_or_else(|| XmppDomain::try_new("localhost").unwrap_or_else(|_| unreachable!())))
}

/// Validates and resolves a tenant's ingress mode and hostnames against its hosting
/// `WaddleServer`'s [`IngressSpec`].
///
/// Enforces lowercase FQDN syntax, rejects apex `managedBaseDomain`, rejects multi-label
/// subdomains under `managedBaseDomain`, and enforces both [`BUILTIN_RESERVED_SUBDOMAINS`]
/// and `server_ingress.reserved_subdomains`.
///
/// # Errors
/// Returns [`TenantIngressError`] if any hostname violates managed-base, reserved-subdomain,
/// or mode invariants.
pub fn validate_tenant_ingress_against_server(
    spec: &WaddleTenantSpec,
    server_ingress: &IngressSpec,
) -> Result<ResolvedTenantIngress, TenantIngressError> {
    let hostnames = spec.effective_hostnames();
    let requested_mode = spec.effective_ingress_mode();

    if !server_ingress.enabled || requested_mode == TenantIngressMode::Disabled {
        return Ok(ResolvedTenantIngress {
            mode: ResolvedIngressMode::Disabled,
            hostnames,
        });
    }

    let managed_base = server_ingress
        .managed_base_domain
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut reserved: BTreeSet<&str> = BUILTIN_RESERVED_SUBDOMAINS.iter().copied().collect();
    for extra in &server_ingress.reserved_subdomains {
        let trimmed = extra.trim();
        if !trimmed.is_empty() {
            reserved.insert(trimmed);
        }
    }

    let mut saw_managed = false;
    let mut saw_custom = false;

    for host in &hostnames {
        if host != host.trim()
            || !host.contains('.')
            || host.parse::<IpAddr>().is_ok()
            || XmppDomain::try_new(host).is_err()
        {
            return Err(TenantIngressError::InvalidHostname {
                hostname: host.clone(),
            });
        }

        if let Some(base) = managed_base {
            if host == base {
                return Err(TenantIngressError::ApexManagedBaseDomainForbidden {
                    hostname: host.clone(),
                    base_domain: base.to_owned(),
                });
            }
            let suffix = format!(".{base}");
            if let Some(prefix) = host.strip_suffix(&suffix) {
                if prefix.contains('.') {
                    return Err(TenantIngressError::MultiLabelManagedSubdomain {
                        hostname: host.clone(),
                        base_domain: base.to_owned(),
                    });
                }
                if reserved.contains(prefix) {
                    return Err(TenantIngressError::ReservedSubdomain {
                        subdomain: prefix.to_owned(),
                        base_domain: base.to_owned(),
                    });
                }
                saw_managed = true;
                continue;
            }
        }

        saw_custom = true;
    }

    let resolved_mode = match requested_mode {
        TenantIngressMode::Disabled => ResolvedIngressMode::Disabled,
        TenantIngressMode::ManagedSubdomain => {
            let Some(base) = managed_base else {
                return Err(TenantIngressError::ManagedBaseDomainNotConfigured);
            };
            if saw_custom {
                let offending = hostnames
                    .iter()
                    .find(|h| !h.ends_with(&format!(".{base}")))
                    .cloned()
                    .unwrap_or_default();
                return Err(TenantIngressError::HostnameNotUnderManagedBaseDomain {
                    hostname: offending,
                    base_domain: base.to_owned(),
                });
            }
            ResolvedIngressMode::ManagedSubdomain
        }
        TenantIngressMode::CustomDomain => {
            if saw_managed {
                let base = managed_base.unwrap_or_default();
                let offending = hostnames
                    .iter()
                    .find(|h| h.ends_with(&format!(".{base}")))
                    .cloned()
                    .unwrap_or_default();
                return Err(TenantIngressError::CustomDomainUnderManagedBaseDomain {
                    hostname: offending,
                    base_domain: base.to_owned(),
                });
            }
            ResolvedIngressMode::CustomDomain
        }
        TenantIngressMode::Auto => match (saw_managed, saw_custom) {
            (true, false) => ResolvedIngressMode::ManagedSubdomain,
            (false, true) => ResolvedIngressMode::CustomDomain,
            (true, true) => {
                return Err(TenantIngressError::MixedIngressHostnameModes {
                    base_domain: managed_base.unwrap_or_default().to_owned(),
                });
            }
            (false, false) => ResolvedIngressMode::Disabled,
        },
    };

    Ok(ResolvedTenantIngress {
        mode: resolved_mode,
        hostnames,
    })
}

#[cfg(test)]
mod tests {
    use kube::CustomResourceExt;
    use serde_json::Value;

    use super::*;

    fn rka_provider() -> TenantAuthProvider {
        TenantAuthProvider {
            id: "rawkode-academy".to_owned(),
            display_name: "Rawkode Academy".to_owned(),
            kind: TenantAuthProviderKind::Oidc,
            dynamic_client_registration: false,
            client_id: "waddle-rawkode-chat".to_owned(),
            require_dpop: false,
            scopes: vec![
                "openid".to_owned(),
                "profile".to_owned(),
                "email".to_owned(),
            ],
            issuer: "https://id.rawkode.academy".to_owned(),
            authorization_endpoint: None,
            token_endpoint: None,
            userinfo_endpoint: None,
            jwks_uri: None,
            subject_claim: None,
            username_claim: Some("preferred_username".to_owned()),
            email_claim: Some("email".to_owned()),
        }
    }

    fn sample_tenant_spec() -> WaddleTenantSpec {
        WaddleTenantSpec {
            server_ref: LocalObjectRef {
                name: "waddle".to_owned(),
            },
            tenant_id: "waddle.social".to_owned(),
            placement_mode: TenantPlacementMode::SharedPool,
            owners: vec![
                "rawkode".to_owned(),
                "icepuma".to_owned(),
                "randax".to_owned(),
            ],
            auth: Some(TenantAuthSpec {
                providers: vec![rka_provider()],
            }),
            quotas: Some(TenantQuotasSpec {
                max_concurrent_connections: Some(1000),
                max_rooms: Some(500),
                max_storage_bytes: Some(10 * 1024 * 1024 * 1024),
                max_inference_spend_microcents: Some(50_000_000),
            }),
            extensions: Some(TenantExtensionsSpec {
                enabled_plugins: vec!["ai".to_owned()],
                approved_providers: vec!["openrouter".to_owned()],
                approved_models: vec!["anthropic/claude-sonnet-4".to_owned()],
                zdr_required: true,
            }),
            ingress: None,
        }
    }

    fn walk_schema_rejecting_non_structural(node: &Value, path: &str) {
        if let Some(obj) = node.as_object() {
            assert!(
                !obj.contains_key("oneOf"),
                "CRD schema at {path} must not contain non-structural oneOf"
            );
            assert!(
                !obj.contains_key("anyOf"),
                "CRD schema at {path} must not contain anyOf"
            );
            assert_ne!(
                obj.get("additionalProperties").and_then(Value::as_bool),
                Some(false),
                "CRD schema at {path} must not emit additionalProperties: false"
            );
            for (k, v) in obj {
                walk_schema_rejecting_non_structural(v, &format!("{path}.{k}"));
            }
        } else if let Some(arr) = node.as_array() {
            for (idx, item) in arr.iter().enumerate() {
                walk_schema_rejecting_non_structural(item, &format!("{path}[{idx}]"));
            }
        }
    }

    #[test]
    fn tenant_crd_schema_is_structural_and_compact() {
        let crd = WaddleTenant::crd();
        let json_val = serde_json::to_value(&crd).expect("CRD serializes to JSON");
        walk_schema_rejecting_non_structural(&json_val, "$");

        let yaml = serde_yaml_ng::to_string(&crd).expect("CRD serializes to YAML");
        assert!(
            yaml.len() < 100 * 1024,
            "WaddleTenant CRD YAML size ({} bytes) exceeds 100 KiB budget",
            yaml.len()
        );
    }

    #[test]
    fn tenant_cel_accepts_canonical_dns_and_rejects_invalid_domains() {
        let valid = WaddleTenant::new("waddle-social", sample_tenant_spec());
        assert!(
            valid.validate_cel().is_ok(),
            "valid WaddleTenant must pass CEL: {:?}",
            valid.validate_cel()
        );

        let mut upper = sample_tenant_spec();
        upper.tenant_id = "Waddle.Social".to_owned();
        assert!(
            WaddleTenant::new("bad-upper", upper)
                .validate_cel()
                .is_err(),
            "uppercase tenantId must be rejected by CEL"
        );

        let mut leading_hyphen = sample_tenant_spec();
        leading_hyphen.tenant_id = "-bad.social".to_owned();
        assert!(
            WaddleTenant::new("bad-hyphen", leading_hyphen)
                .validate_cel()
                .is_err(),
            "leading hyphen tenantId must be rejected by CEL"
        );
    }

    #[test]
    fn pure_tenant_validation_catches_invalid_refs_and_empty_owners() {
        assert!(validate_tenant_spec(&sample_tenant_spec()).is_ok());

        let mut bad = sample_tenant_spec();
        bad.server_ref.name = "INVALID_NAME".to_owned();
        bad.tenant_id = "https://waddle.social".to_owned();
        bad.owners.push("   ".to_owned());
        let mut no_openid = rka_provider();
        no_openid.id = "no-openid".to_owned();
        no_openid.scopes = vec!["profile".to_owned()];
        bad.auth = Some(TenantAuthSpec {
            providers: vec![rka_provider(), rka_provider(), no_openid],
        });

        let report = validate_tenant_spec(&bad).expect_err("invalid tenant spec must fail");
        assert!(report
            .violations()
            .contains(&TenantValidationViolation::InvalidServerRefName));
        assert!(report
            .violations()
            .contains(&TenantValidationViolation::InvalidTenantId));
        assert!(report
            .violations()
            .contains(&TenantValidationViolation::EmptyOwnerEntry));
        assert!(report
            .violations()
            .contains(&TenantValidationViolation::InvalidAuthProvider));
        assert!(report
            .violations()
            .contains(&TenantValidationViolation::DuplicateAuthProviderId));
    }

    #[test]
    fn tenant_auth_provider_validation_rejects_incomplete_or_insecure_providers() {
        type Mutation = fn(&mut TenantAuthProvider);
        let cases: Vec<(&str, Mutation)> = vec![
            ("plain http issuer", |p| {
                p.issuer = "http://id.rawkode.academy".to_owned()
            }),
            ("static client without client id", |p| p.client_id.clear()),
            ("empty display name", |p| p.display_name = " ".to_owned()),
            ("oauth2 without endpoints", |p| {
                p.kind = TenantAuthProviderKind::OAuth2;
            }),
            ("insecure jwks override", |p| {
                p.jwks_uri = Some("http://id.rawkode.academy/jwks".to_owned());
            }),
        ];
        for (name, mutate) in cases {
            let mut provider = rka_provider();
            mutate(&mut provider);
            assert!(!provider.is_valid(), "{name} must be rejected");
        }

        let mut dcr = rka_provider();
        dcr.client_id.clear();
        dcr.dynamic_client_registration = true;
        assert!(dcr.is_valid(), "dynamic registration needs no client id");
    }

    #[test]
    fn tenant_auth_provider_serializes_to_the_server_manifest_shape() {
        let json = serde_json::to_value(rka_provider()).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "id": "rawkode-academy",
                "displayName": "Rawkode Academy",
                "kind": "oidc",
                "clientId": "waddle-rawkode-chat",
                "scopes": ["openid", "profile", "email"],
                "issuer": "https://id.rawkode.academy",
                "usernameClaim": "preferred_username",
                "emailClaim": "email"
            })
        );
    }

    fn enabled_server_ingress() -> IngressSpec {
        IngressSpec {
            enabled: true,
            gateway_ref: Some(crate::server_config::GatewayRefSpec {
                name: "waddle-gateway".to_owned(),
                namespace: "default".to_owned(),
            }),
            managed_base_domain: Some("waddle.social".to_owned()),
            managed_wildcard_https_section: Some("https-waddle-tenants".to_owned()),
            managed_wildcard_http_section: Some("http-waddle-tenants".to_owned()),
            byo_http_section: Some("http-byo-tenants".to_owned()),
            target_ips: vec!["51.159.109.202".to_owned()],
            tls_cluster_issuer: Some("letsencrypt-production".to_owned()),
            reserved_subdomains: vec!["billing".to_owned()],
            ..IngressSpec::default()
        }
    }

    #[test]
    fn tenant_ingress_validation_handles_managed_custom_disabled_and_rejects_reserved_and_apex() {
        let server = enabled_server_ingress();

        // Disabled mode allows apex + preview hostnames to be projected without routing mutations.
        let mut disabled = sample_tenant_spec();
        disabled.ingress = Some(TenantIngressSpec {
            mode: TenantIngressMode::Disabled,
            hostnames: vec![
                "waddle.social".to_owned(),
                "next.apps.waddle.chat".to_owned(),
            ],
        });
        let resolved =
            validate_tenant_ingress_against_server(&disabled, &server).expect("disabled valid");
        assert_eq!(resolved.mode, ResolvedIngressMode::Disabled);
        assert_eq!(
            resolved.hostnames,
            vec!["waddle.social", "next.apps.waddle.chat"]
        );

        // Managed subdomain resolves cleanly.
        let mut managed = sample_tenant_spec();
        managed.tenant_id = "example.waddle.social".to_owned();
        managed.ingress = Some(TenantIngressSpec {
            mode: TenantIngressMode::ManagedSubdomain,
            hostnames: Vec::new(),
        });
        let resolved =
            validate_tenant_ingress_against_server(&managed, &server).expect("managed valid");
        assert_eq!(resolved.mode, ResolvedIngressMode::ManagedSubdomain);
        assert_eq!(resolved.hostnames, vec!["example.waddle.social"]);

        // Custom domain resolves cleanly (both explicit and Auto).
        let mut custom = sample_tenant_spec();
        custom.tenant_id = "waddle.rawkode.chat".to_owned();
        custom.ingress = Some(TenantIngressSpec {
            mode: TenantIngressMode::CustomDomain,
            hostnames: Vec::new(),
        });
        let resolved =
            validate_tenant_ingress_against_server(&custom, &server).expect("custom valid");
        assert_eq!(resolved.mode, ResolvedIngressMode::CustomDomain);
        assert_eq!(resolved.hostnames, vec!["waddle.rawkode.chat"]);

        // Apex managedBaseDomain is rejected for ManagedSubdomain and Auto.
        let mut apex = sample_tenant_spec();
        apex.tenant_id = "waddle.social".to_owned();
        apex.ingress = Some(TenantIngressSpec {
            mode: TenantIngressMode::ManagedSubdomain,
            hostnames: Vec::new(),
        });
        assert!(matches!(
            validate_tenant_ingress_against_server(&apex, &server),
            Err(TenantIngressError::ApexManagedBaseDomainForbidden { .. })
        ));

        // Multi-label subdomain under managedBaseDomain is rejected.
        let mut multi = sample_tenant_spec();
        multi.tenant_id = "foo.teleport.waddle.social".to_owned();
        multi.ingress = Some(TenantIngressSpec {
            mode: TenantIngressMode::Auto,
            hostnames: Vec::new(),
        });
        assert!(matches!(
            validate_tenant_ingress_against_server(&multi, &server),
            Err(TenantIngressError::MultiLabelManagedSubdomain { .. })
        ));

        // Every built-in reserved subdomain and server-configured reserved subdomain is rejected.
        for label in BUILTIN_RESERVED_SUBDOMAINS
            .iter()
            .copied()
            .chain(std::iter::once("billing"))
        {
            let mut res = sample_tenant_spec();
            res.tenant_id = format!("{label}.waddle.social");
            res.ingress = Some(TenantIngressSpec {
                mode: TenantIngressMode::ManagedSubdomain,
                hostnames: Vec::new(),
            });
            assert!(
                matches!(
                    validate_tenant_ingress_against_server(&res, &server),
                    Err(TenantIngressError::ReservedSubdomain { .. })
                ),
                "reserved subdomain {label}.waddle.social must be rejected"
            );
        }

        // CustomDomain under managedBaseDomain is rejected.
        let mut custom_under_base = sample_tenant_spec();
        custom_under_base.tenant_id = "example.waddle.social".to_owned();
        custom_under_base.ingress = Some(TenantIngressSpec {
            mode: TenantIngressMode::CustomDomain,
            hostnames: Vec::new(),
        });
        assert!(matches!(
            validate_tenant_ingress_against_server(&custom_under_base, &server),
            Err(TenantIngressError::CustomDomainUnderManagedBaseDomain { .. })
        ));
    }

    #[test]
    fn tenant_cel_rejects_wildcard_and_uppercase_ingress_hostnames() {
        let mut wildcard = sample_tenant_spec();
        wildcard.ingress = Some(TenantIngressSpec {
            mode: TenantIngressMode::CustomDomain,
            hostnames: vec!["*.rawkode.chat".to_owned()],
        });
        assert!(WaddleTenant::new("bad-wild", wildcard)
            .validate_cel()
            .is_err());

        let mut upper = sample_tenant_spec();
        upper.ingress = Some(TenantIngressSpec {
            mode: TenantIngressMode::CustomDomain,
            hostnames: vec!["Waddle.Rawkode.Chat".to_owned()],
        });
        assert!(WaddleTenant::new("bad-upper-host", upper)
            .validate_cel()
            .is_err());
    }

    #[test]
    fn crd_schemas_bound_cel_validated_fields() {
        let tenant_crd = serde_json::to_value(<WaddleTenant as kube::CustomResourceExt>::crd())
            .expect("serialize WaddleTenant CRD");
        let tenant_spec = &tenant_crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]
            ["properties"]["spec"]["properties"];
        assert_eq!(tenant_spec["tenantId"]["maxLength"], 253);
        let hostnames = &tenant_spec["ingress"]["properties"]["hostnames"];
        assert_eq!(hostnames["maxItems"], 16);
        assert_eq!(hostnames["items"]["maxLength"], 253);

        let server_crd =
            serde_json::to_value(<crate::WaddleServer as kube::CustomResourceExt>::crd())
                .expect("serialize WaddleServer CRD");
        let server_ingress = &server_crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]
            ["properties"]["spec"]["properties"]["ingress"]["properties"];
        assert_eq!(server_ingress["reservedSubdomains"]["maxItems"], 256);
        assert_eq!(
            server_ingress["reservedSubdomains"]["items"]["maxLength"],
            63
        );
        assert_eq!(server_ingress["targetIps"]["maxItems"], 16);
        assert_eq!(server_ingress["targetIps"]["items"]["maxLength"], 45);
    }
}
