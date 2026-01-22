//! DNS SRV record resolution for S2S federation discovery.
//!
//! This module implements DNS SRV record lookup for XMPP server-to-server
//! connections as specified in RFC 6120 Section 3.2.
//!
//! The resolution process:
//! 1. Query for `_xmpp-server._tcp.{domain}` SRV records
//! 2. Sort results by priority (ascending) and weight (descending within same priority)
//! 3. Fall back to A/AAAA records on port 5269 if no SRV records exist
//!
//! # Example
//!
//! ```ignore
//! use waddle_xmpp::s2s::dns::SrvResolver;
//!
//! let resolver = SrvResolver::new().await?;
//! let targets = resolver.resolve_xmpp_server("example.com").await?;
//! for target in targets {
//!     println!("{}:{}", target.host, target.port);
//! }
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::rr::rdata::SRV;
use hickory_resolver::proto::ProtoErrorKind;
use hickory_resolver::{ResolveError, ResolveErrorKind, Resolver};
use thiserror::Error;
use tracing::{debug, instrument, warn};

/// Default XMPP S2S port as per RFC 6120.
pub const DEFAULT_S2S_PORT: u16 = 5269;

/// DNS resolution errors.
#[derive(Debug, Error)]
pub enum DnsError {
    /// Failed to create DNS resolver.
    #[error("failed to create DNS resolver: {0}")]
    ResolverCreation(#[from] ResolveError),

    /// No records found for the domain.
    #[error("no DNS records found for domain: {0}")]
    NoRecords(String),

    /// Domain resolution failed.
    #[error("DNS resolution failed for {domain}: {message}")]
    ResolutionFailed { domain: String, message: String },
}

/// A resolved XMPP server target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// The hostname or IP address.
    pub host: String,
    /// The port number.
    pub port: u16,
    /// The SRV priority (lower is higher priority).
    pub priority: u16,
    /// The SRV weight (higher gets more traffic within same priority).
    pub weight: u16,
}

impl ResolvedTarget {
    /// Create a new resolved target.
    pub fn new(host: String, port: u16, priority: u16, weight: u16) -> Self {
        Self {
            host,
            port,
            priority,
            weight,
        }
    }

    /// Create a fallback target (for A/AAAA records).
    pub fn fallback(host: String) -> Self {
        Self {
            host,
            port: DEFAULT_S2S_PORT,
            priority: 0,
            weight: 0,
        }
    }

    /// Convert to a socket address by resolving the hostname.
    ///
    /// Returns all resolved addresses for the target.
    pub async fn to_socket_addrs(
        &self,
        resolver: &SrvResolver,
    ) -> Result<Vec<SocketAddr>, DnsError> {
        resolver.resolve_host_to_addrs(&self.host, self.port).await
    }
}

/// Type alias for the Tokio-based resolver.
pub type TokioResolver = Resolver<TokioConnectionProvider>;

/// DNS SRV resolver for XMPP S2S federation.
///
/// This resolver handles the complete DNS resolution process for XMPP
/// server-to-server connections, including SRV record lookup with
/// proper priority/weight sorting and fallback to A/AAAA records.
#[derive(Clone)]
pub struct SrvResolver {
    resolver: Arc<TokioResolver>,
}

impl SrvResolver {
    /// Create a new SRV resolver with system DNS configuration.
    pub async fn new() -> Result<Self, DnsError> {
        let resolver = Resolver::builder_with_config(
            ResolverConfig::default(),
            TokioConnectionProvider::default(),
        )
        .build();
        Ok(Self {
            resolver: Arc::new(resolver),
        })
    }

    /// Create a new SRV resolver with custom configuration.
    pub fn with_config(config: ResolverConfig, opts: ResolverOpts) -> Self {
        let resolver = Resolver::builder_with_config(config, TokioConnectionProvider::default())
            .with_options(opts)
            .build();
        Self {
            resolver: Arc::new(resolver),
        }
    }

    /// Resolve XMPP server targets for a domain.
    ///
    /// This implements the XMPP S2S DNS resolution process:
    /// 1. Query `_xmpp-server._tcp.{domain}` for SRV records
    /// 2. Sort by priority (ascending) and weight (descending)
    /// 3. Fall back to A/AAAA on port 5269 if no SRV records
    ///
    /// # Arguments
    ///
    /// * `domain` - The domain to resolve (e.g., "example.com")
    ///
    /// # Returns
    ///
    /// A list of resolved targets sorted by priority and weight.
    #[instrument(skip(self), name = "dns.resolve_xmpp_server")]
    pub async fn resolve_xmpp_server(&self, domain: &str) -> Result<Vec<ResolvedTarget>, DnsError> {
        let srv_name = format!("_xmpp-server._tcp.{}", domain);
        debug!(srv_name = %srv_name, "Resolving XMPP S2S SRV records");

        // Try SRV lookup first
        match self.resolver.srv_lookup(&srv_name).await {
            Ok(srv_response) => {
                let records: Vec<&SRV> = srv_response.iter().collect();

                if records.is_empty() {
                    debug!("No SRV records found, falling back to A/AAAA");
                    return self.resolve_fallback(domain).await;
                }

                let mut targets = self.process_srv_records(&records);

                if targets.is_empty() {
                    debug!("SRV records returned no valid targets, falling back to A/AAAA");
                    return self.resolve_fallback(domain).await;
                }

                // Sort by priority (ascending), then by weight (descending)
                targets.sort_by(|a, b| {
                    match a.priority.cmp(&b.priority) {
                        std::cmp::Ordering::Equal => b.weight.cmp(&a.weight),
                        other => other,
                    }
                });

                debug!(
                    count = targets.len(),
                    "Resolved {} XMPP S2S targets via SRV",
                    targets.len()
                );

                Ok(targets)
            }
            Err(e) => {
                // Check if it's a "no records" error vs a real failure
                if is_no_records_error(&e) {
                    debug!("No SRV records exist, falling back to A/AAAA");
                    self.resolve_fallback(domain).await
                } else {
                    warn!(error = %e, "SRV lookup failed, falling back to A/AAAA");
                    self.resolve_fallback(domain).await
                }
            }
        }
    }

    /// Process SRV records into resolved targets.
    fn process_srv_records(&self, records: &[&SRV]) -> Vec<ResolvedTarget> {
        records
            .iter()
            .filter_map(|srv| {
                let target = srv.target().to_utf8();
                // Skip the root target "." which means "no service available"
                if target == "." || target.is_empty() {
                    return None;
                }
                // Remove trailing dot if present
                let host = target.trim_end_matches('.');
                Some(ResolvedTarget::new(
                    host.to_string(),
                    srv.port(),
                    srv.priority(),
                    srv.weight(),
                ))
            })
            .collect()
    }

    /// Resolve fallback using A/AAAA records on port 5269.
    #[instrument(skip(self), name = "dns.resolve_fallback")]
    async fn resolve_fallback(&self, domain: &str) -> Result<Vec<ResolvedTarget>, DnsError> {
        debug!(domain = %domain, "Attempting A/AAAA fallback resolution");

        // Try to lookup the domain directly
        match self.resolver.lookup_ip(domain).await {
            Ok(response) => {
                let addrs: Vec<_> = response.iter().collect();
                if addrs.is_empty() {
                    return Err(DnsError::NoRecords(domain.to_string()));
                }

                debug!(
                    count = addrs.len(),
                    "Resolved {} IP addresses for fallback",
                    addrs.len()
                );

                // Return the domain as the target (not individual IPs)
                // This allows the connection code to handle IP selection
                Ok(vec![ResolvedTarget::fallback(domain.to_string())])
            }
            Err(e) => {
                warn!(error = %e, domain = %domain, "Fallback A/AAAA lookup failed");
                Err(DnsError::ResolutionFailed {
                    domain: domain.to_string(),
                    message: e.to_string(),
                })
            }
        }
    }

    /// Resolve a hostname to socket addresses.
    ///
    /// This is used after SRV resolution to get actual IP addresses
    /// for connection attempts.
    #[instrument(skip(self), name = "dns.resolve_host")]
    pub async fn resolve_host_to_addrs(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<SocketAddr>, DnsError> {
        match self.resolver.lookup_ip(host).await {
            Ok(response) => {
                let addrs: Vec<SocketAddr> = response
                    .iter()
                    .map(|ip| SocketAddr::new(ip, port))
                    .collect();

                if addrs.is_empty() {
                    return Err(DnsError::NoRecords(host.to_string()));
                }

                debug!(
                    host = %host,
                    port = port,
                    count = addrs.len(),
                    "Resolved host to {} addresses",
                    addrs.len()
                );

                Ok(addrs)
            }
            Err(e) => Err(DnsError::ResolutionFailed {
                domain: host.to_string(),
                message: e.to_string(),
            }),
        }
    }
}

/// Check if a resolve error indicates no records exist.
///
/// In hickory-resolver 0.25, NoRecordsFound is in ProtoErrorKind,
/// not ResolveErrorKind. We need to check the inner Proto error.
fn is_no_records_error(error: &ResolveError) -> bool {
    if let ResolveErrorKind::Proto(proto_error) = error.kind() {
        matches!(proto_error.kind(), ProtoErrorKind::NoRecordsFound { .. })
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolved_target_new() {
        let target = ResolvedTarget::new("xmpp.example.com".to_string(), 5269, 10, 50);
        assert_eq!(target.host, "xmpp.example.com");
        assert_eq!(target.port, 5269);
        assert_eq!(target.priority, 10);
        assert_eq!(target.weight, 50);
    }

    #[test]
    fn test_resolved_target_fallback() {
        let target = ResolvedTarget::fallback("example.com".to_string());
        assert_eq!(target.host, "example.com");
        assert_eq!(target.port, DEFAULT_S2S_PORT);
        assert_eq!(target.priority, 0);
        assert_eq!(target.weight, 0);
    }

    #[test]
    fn test_target_sorting() {
        // Create targets with different priorities and weights
        let mut targets = vec![
            ResolvedTarget::new("c.example.com".to_string(), 5269, 20, 50),
            ResolvedTarget::new("a.example.com".to_string(), 5269, 10, 30),
            ResolvedTarget::new("b.example.com".to_string(), 5269, 10, 70),
            ResolvedTarget::new("d.example.com".to_string(), 5269, 30, 100),
        ];

        // Sort by priority (ascending), then weight (descending)
        targets.sort_by(|a, b| {
            match a.priority.cmp(&b.priority) {
                std::cmp::Ordering::Equal => b.weight.cmp(&a.weight),
                other => other,
            }
        });

        // Priority 10 should come first, with higher weight first
        assert_eq!(targets[0].host, "b.example.com"); // priority 10, weight 70
        assert_eq!(targets[1].host, "a.example.com"); // priority 10, weight 30
        assert_eq!(targets[2].host, "c.example.com"); // priority 20, weight 50
        assert_eq!(targets[3].host, "d.example.com"); // priority 30, weight 100
    }

    #[test]
    fn test_default_s2s_port() {
        assert_eq!(DEFAULT_S2S_PORT, 5269);
    }

    #[tokio::test]
    async fn test_resolver_creation() {
        // Test that we can create a resolver
        let result = SrvResolver::new().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_resolve_nonexistent_domain() {
        let resolver = SrvResolver::new().await.unwrap();
        // Use a domain that definitely won't exist
        let result = resolver
            .resolve_xmpp_server("nonexistent.invalid.test.domain.local")
            .await;
        // Should fail with no records or resolution failed
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resolve_well_known_domain() {
        // Test against a well-known XMPP server that should have SRV records
        // Note: This test requires network access and may be flaky
        let resolver = SrvResolver::new().await.unwrap();

        // jabber.org is a well-known XMPP server with SRV records
        // If this fails, it might be due to network issues or the domain changing
        let result = resolver.resolve_xmpp_server("jabber.org").await;

        // We expect this to succeed (either SRV or fallback)
        // but don't assert on specific values as they may change
        match result {
            Ok(targets) => {
                assert!(!targets.is_empty(), "Should have at least one target");
                for target in &targets {
                    assert!(!target.host.is_empty(), "Host should not be empty");
                    assert!(target.port > 0, "Port should be positive");
                }
            }
            Err(e) => {
                // Network tests can be flaky, so we'll just log the error
                eprintln!("Network test failed (may be expected in CI): {}", e);
            }
        }
    }
}
