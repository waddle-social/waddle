//! DNS-resolved SSRF IP blocklist using `waddle_broker_core::kernels::ssrf_ip_filter`.

use std::net::{IpAddr, SocketAddr};

use async_trait::async_trait;
use thiserror::Error;
use waddle_broker_core::kernels::ssrf_ip_filter::is_blocked_ip;

/// Injectable DNS resolution abstraction so egress SSRF validation can be
/// tested deterministically with adversarial DNS responses as well as real
/// `tokio::net::lookup_host`.
#[async_trait]
pub trait DnsResolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, SsrfError>;
}

/// Production DNS resolver backed by `tokio::net::lookup_host`.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioDnsResolver;

#[async_trait]
impl DnsResolver for TokioDnsResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, SsrfError> {
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| SsrfError::ResolutionFailed)?;
        Ok(addrs.collect())
    }
}

/// Errors returned by DNS-resolved SSRF IP validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SsrfError {
    #[error("target URL has no valid host or port")]
    MissingHost,
    #[error("literal IP hosts must not bypass domain validation unless explicitly permitted")]
    BlockedIpAddress(IpAddr),
    #[error("DNS resolution failed")]
    ResolutionFailed,
    #[error("DNS resolution returned zero addresses")]
    EmptyResolution,
}

/// Resolves `(host, port)` and validates that the resolved list is non-empty and
/// contains ZERO blocked IPs (loopback, RFC1918, CGNAT `100.64.0.0/10`,
/// link-local/IMDS `169.254.169.254`, IPv6 `::`, `::1`, ULA `fc00::/7`,
/// link-local `fe80::/10`, multicast, and IPv4-mapped IPv6 `::ffff:0:0/96`).
///
/// Even if one returned A/AAAA record is public, ANY blocked IP in the answer
/// set causes fail-closed rejection to prevent DNS-rebinding/multi-record SSRF.
pub async fn resolve_and_validate_addrs(
    resolver: &dyn DnsResolver,
    host: &str,
    port: u16,
) -> Result<Vec<SocketAddr>, SsrfError> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err(SsrfError::MissingHost);
    }
    let bare = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(SsrfError::BlockedIpAddress(ip));
        }
        return Ok(vec![SocketAddr::new(ip, port)]);
    }

    let addrs = resolver.resolve(bare, port).await?;
    if addrs.is_empty() {
        return Err(SsrfError::EmptyResolution);
    }
    for addr in &addrs {
        if is_blocked_ip(addr.ip()) {
            return Err(SsrfError::BlockedIpAddress(addr.ip()));
        }
    }
    Ok(addrs)
}
