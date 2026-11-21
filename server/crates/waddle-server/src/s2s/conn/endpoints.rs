//! Where an outbound connection to a remote domain goes.
//!
//! The connection manager asks an [`EndpointResolver`] for the endpoints of
//! a remote domain and tries them in order. An endpoint selects a socket
//! and how to reach TLS on it only: the remote domain stays the TLS server
//! name and the certificate's reference identity, whatever the endpoint
//! is. [`ConfiguredEndpoints`] resolves the endpoints the configuration
//! lists; DNS discovery (`crate::s2s::discovery`) implements the same trait
//! and is consulted only for a domain without configured endpoints.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use waddle_xmpp::identity::CanonicalDomain;

use crate::s2s::config::S2sConfig;
use crate::s2s::verify::OutboundTransport;

/// How an endpoint was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointOrigin {
    /// Operator configuration (RFC 6120 §3.2.3); exempt from the address
    /// checks DNS-derived addresses get.
    Configured,
    /// An `_xmpps-server` or `_xmpp-server` SRV target's address.
    Srv,
    /// The domain's own A or AAAA record on port 5269, after both SRV
    /// lookups answered that no record exists.
    Fallback,
}

impl EndpointOrigin {
    /// Whether the address came from DNS, so the connection manager checks
    /// it against the blocked address classes again on the address it
    /// connects to.
    pub fn is_dns(self) -> bool {
        matches!(self, Self::Srv | Self::Fallback)
    }
}

/// One address to try, and how to reach TLS on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub address: SocketAddr,
    pub origin: EndpointOrigin,
    /// STARTTLS for `_xmpp-server` targets, the A/AAAA fallback and
    /// configured STARTTLS endpoints; direct TLS (XEP-0368) for
    /// `_xmpps-server` targets and configured direct-TLS endpoints.
    pub transport: OutboundTransport,
}

/// The endpoints of one remote domain, in the order they are tried.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Endpoints(Vec<Endpoint>);

impl Endpoints {
    pub fn new(endpoints: Vec<Endpoint>) -> Self {
        Self(endpoints)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Endpoint> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// Why a remote domain has no endpoint to try.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ResolveError {
    /// Nothing is configured or published for the domain, or DNS says the
    /// service is not offered (a "." SRV target without usable records).
    #[error("the remote domain has no endpoint")]
    NoEndpoints,
    /// A lookup failed; this is not the absence of records. The attempt
    /// fails and is retried later with backoff, without A/AAAA fallback.
    #[error("the endpoint lookup failed")]
    LookupFailed,
    /// DNS returned addresses, and every one is in a blocked class.
    #[error("every address of the remote domain is in a blocked class")]
    Blocked,
    /// The domain is an IP address, a numeric IPv4 form or a single-label
    /// name, which is never resolved or dialled.
    #[error("the remote domain cannot be resolved")]
    NotAPeerDomain,
}

/// Finds the endpoints of a remote domain.
#[async_trait]
pub trait EndpointResolver: Send + Sync {
    async fn resolve(&self, domain: &CanonicalDomain) -> Result<Endpoints, ResolveError>;
}

/// Resolves only the endpoints the configuration snapshot lists.
#[derive(Debug, Clone)]
pub struct ConfiguredEndpoints {
    config: Arc<S2sConfig>,
}

impl ConfiguredEndpoints {
    pub fn new(config: Arc<S2sConfig>) -> Self {
        Self { config }
    }
}

#[async_trait]
impl EndpointResolver for ConfiguredEndpoints {
    async fn resolve(&self, domain: &CanonicalDomain) -> Result<Endpoints, ResolveError> {
        let endpoints: Vec<Endpoint> = self
            .config
            .endpoints(domain)
            .iter()
            .map(|endpoint| Endpoint {
                address: endpoint.address,
                origin: EndpointOrigin::Configured,
                transport: endpoint.transport,
            })
            .collect();
        if endpoints.is_empty() {
            return Err(ResolveError::NoEndpoints);
        }
        Ok(Endpoints::new(endpoints))
    }
}
