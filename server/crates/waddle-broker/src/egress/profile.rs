//! Approved provider, deployment, model, and exact operation profile registry
//! with unconditional blocking of provider admin and credential inspection paths.

use std::collections::BTreeMap;

use thiserror::Error;

/// Allowed inference operations (strictly inference-only; never admin/key/org).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderOperation {
    ChatCompletions,
    Responses,
    Messages,
}

impl ProviderOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }
}

/// Resource, byte, concurrency, deadline, and spend caps enforced on broker egress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressCaps {
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_chunk_bytes: usize,
    pub request_timeout_ms: u64,
    pub max_concurrent_requests: usize,
    pub max_spend_microcents: u64,
}

impl Default for EgressCaps {
    fn default() -> Self {
        Self {
            max_request_bytes: 256 * 1024,
            max_response_bytes: 2 * 1024 * 1024,
            max_chunk_bytes: 64 * 1024,
            request_timeout_ms: 30_000,
            max_concurrent_requests: 16,
            max_spend_microcents: 10_000_000,
        }
    }
}

impl EgressCaps {
    pub fn validate(&self) -> Result<(), ProfileError> {
        if self.max_request_bytes == 0 {
            return Err(ProfileError::InvalidCaps("max_request_bytes must be > 0"));
        }
        if self.max_response_bytes == 0 {
            return Err(ProfileError::InvalidCaps("max_response_bytes must be > 0"));
        }
        if self.max_chunk_bytes == 0 || self.max_chunk_bytes > self.max_response_bytes {
            return Err(ProfileError::InvalidCaps(
                "max_chunk_bytes must be in 1..=max_response_bytes",
            ));
        }
        if self.request_timeout_ms == 0 {
            return Err(ProfileError::InvalidCaps("request_timeout_ms must be > 0"));
        }
        if self.max_concurrent_requests == 0 {
            return Err(ProfileError::InvalidCaps(
                "max_concurrent_requests must be > 0",
            ));
        }
        Ok(())
    }
}

/// A single approved `(provider, deployment, model, operation)` route profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    pub provider: String,
    pub deployment: String,
    pub model: String,
    pub operation: ProviderOperation,
    /// HTTPS base origin (`https://host[:port]`) — never a path or userinfo.
    pub base_origin: String,
    /// Exact operation path (must begin with `/` and must never match an admin path).
    pub operation_path: String,
    /// Optional explicit PEM-encoded CA certificate(s) for pinned/test TLS endpoints.
    ///
    /// When `Some`, ONLY the pinned root certificate(s) are trusted (system/built-in
    /// roots are disabled via `tls_certs_only`). When `None`, standard system/built-in
    /// roots are trusted.
    pub pinned_ca_pem: Option<Vec<u8>>,
    pub caps: EgressCaps,
}

/// Optional additive dispatch-binding policy attached to a registered profile.
///
/// Allows scoping a registered provider profile to a specific trust domain,
/// extension IDs, and/or extension SHA-256 digests without changing
/// [`ProviderProfile`] struct fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EgressBindingPolicy {
    pub expected_trust_domain: Option<String>,
    pub allowed_extension_ids: Vec<String>,
    pub allowed_extension_digests: Vec<[u8; 32]>,
}

/// Errors raised while registering or resolving a provider egress profile.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProfileError {
    #[error("provider, deployment, and model must be non-empty")]
    EmptyField,
    #[error("provider profile origin must be HTTPS domain without literal IP, userinfo, or query")]
    InvalidOrigin,
    #[error("provider admin or credential inspection endpoint is unconditionally blocked")]
    BlockedAdminEndpoint,
    #[error("provider profile path is invalid: {0}")]
    InvalidPath(&'static str),
    #[error("invalid egress caps: {0}")]
    InvalidCaps(&'static str),
    #[error("unapproved egress tuple ({provider}, {deployment}, {model}, {operation})")]
    UnapprovedProfile {
        provider: String,
        deployment: String,
        model: String,
        operation: &'static str,
    },
}

/// Unconditionally blocked path segments/prefixes corresponding to provider
/// administration, billing, or credential inspection APIs.
const BLOCKED_ADMIN_PATH_SEGMENTS: &[&str] = &[
    "/admin",
    "/v1/admin",
    "/v1/organization",
    "/v1/organizations",
    "/v1/keys",
    "/v1/api_keys",
    "/v1/api-keys",
    "/v1/auth",
    "/v1/tokens",
    "/v1/billing",
    "/v1/usage",
    "/v1/users",
    "/v1/invites",
    "/v1/projects",
    "/keys",
    "/api_keys",
    "/api-keys",
    "/tokens",
    "/billing",
    "/organization",
    "/organizations",
    "/auth",
    "/credentials",
    "/v1/credentials",
];

/// Exact approved operation paths permitted on broker egress.
const ALLOWED_EXACT_PATHS: &[&str] = &[
    "/v1/chat/completions",
    "/v1/responses",
    "/v1/messages",
    "/api/v1/chat/completions",
];

/// Returns `true` if `path` matches or descends from any provider admin, billing,
/// or credential inspection endpoint.
#[must_use]
pub fn is_blocked_admin_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return false;
    }
    let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    let normalized = without_query.to_ascii_lowercase();
    for blocked in BLOCKED_ADMIN_PATH_SEGMENTS {
        if normalized == *blocked
            || normalized.starts_with(&format!("{blocked}/"))
            || normalized.contains(&format!("{blocked}/"))
            || normalized.ends_with(blocked)
        {
            return true;
        }
    }
    // Also block path traversal or encoded traversal attempts.
    if normalized.contains("..") || normalized.contains("%2e") {
        return true;
    }
    false
}

/// Validates that `path` is an exact approved inference operation path and not
/// an admin or credential inspection endpoint.
pub fn validate_operation_path(
    path: &str,
    operation: ProviderOperation,
) -> Result<(), ProfileError> {
    if is_blocked_admin_path(path) {
        return Err(ProfileError::BlockedAdminEndpoint);
    }
    if !path.starts_with('/') || path.contains('?') || path.contains('#') || path.contains('\\') {
        return Err(ProfileError::InvalidPath(
            "operation_path must be a clean canonical path starting with '/'",
        ));
    }
    if !ALLOWED_EXACT_PATHS.contains(&path) {
        return Err(ProfileError::InvalidPath(
            "operation_path is not in exact approved inference path allowlist",
        ));
    }
    let matches_op = match operation {
        ProviderOperation::ChatCompletions => {
            path == "/v1/chat/completions" || path == "/api/v1/chat/completions"
        }
        ProviderOperation::Responses => path == "/v1/responses",
        ProviderOperation::Messages => path == "/v1/messages",
    };
    if !matches_op {
        return Err(ProfileError::InvalidPath(
            "operation_path does not match declared ProviderOperation",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ProfileKey {
    provider: String,
    deployment: String,
    model: String,
    operation: ProviderOperation,
}

/// Registry of approved `(provider, deployment, model, operation)` profiles.
#[derive(Debug, Clone, Default)]
pub struct EgressProfileRegistry {
    profiles: BTreeMap<ProfileKey, ProviderProfile>,
    policies: BTreeMap<ProfileKey, EgressBindingPolicy>,
}

impl EgressProfileRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, profile: ProviderProfile) -> Result<(), ProfileError> {
        self.register_with_policy(profile, EgressBindingPolicy::default())
    }

    pub fn register_with_policy(
        &mut self,
        mut profile: ProviderProfile,
        policy: EgressBindingPolicy,
    ) -> Result<(), ProfileError> {
        if profile.provider.trim().is_empty()
            || profile.deployment.trim().is_empty()
            || profile.model.trim().is_empty()
        {
            return Err(ProfileError::EmptyField);
        }
        profile.caps.validate()?;
        validate_operation_path(&profile.operation_path, profile.operation)?;

        let parsed =
            url::Url::parse(&profile.base_origin).map_err(|_| ProfileError::InvalidOrigin)?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || (parsed.path() != "/" && !parsed.path().is_empty())
        {
            return Err(ProfileError::InvalidOrigin);
        }
        if matches!(
            parsed.host(),
            Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) | None
        ) {
            return Err(ProfileError::InvalidOrigin);
        }
        let host = parsed.host_str().ok_or(ProfileError::InvalidOrigin)?;
        profile.base_origin = match parsed.port() {
            Some(port) => format!("https://{host}:{port}"),
            None => format!("https://{host}"),
        };

        let key = ProfileKey {
            provider: profile.provider.clone(),
            deployment: profile.deployment.clone(),
            model: profile.model.clone(),
            operation: profile.operation,
        };
        self.profiles.insert(key.clone(), profile);
        self.policies.insert(key, policy);
        Ok(())
    }

    pub fn resolve(
        &self,
        provider: &str,
        deployment: &str,
        model: &str,
        operation: ProviderOperation,
    ) -> Result<&ProviderProfile, ProfileError> {
        let key = ProfileKey {
            provider: provider.to_owned(),
            deployment: deployment.to_owned(),
            model: model.to_owned(),
            operation,
        };
        self.profiles
            .get(&key)
            .ok_or_else(|| ProfileError::UnapprovedProfile {
                provider: provider.to_owned(),
                deployment: deployment.to_owned(),
                model: model.to_owned(),
                operation: operation.as_str(),
            })
    }

    pub fn resolve_with_policy(
        &self,
        provider: &str,
        deployment: &str,
        model: &str,
        operation: ProviderOperation,
    ) -> Result<(&ProviderProfile, Option<&EgressBindingPolicy>), ProfileError> {
        let key = ProfileKey {
            provider: provider.to_owned(),
            deployment: deployment.to_owned(),
            model: model.to_owned(),
            operation,
        };
        let profile =
            self.profiles
                .get(&key)
                .ok_or_else(|| ProfileError::UnapprovedProfile {
                    provider: provider.to_owned(),
                    deployment: deployment.to_owned(),
                    model: model.to_owned(),
                    operation: operation.as_str(),
                })?;
        Ok((profile, self.policies.get(&key)))
    }
}
