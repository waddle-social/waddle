//! Where an outbound connection to a remote domain goes.
//!
//! The connection manager asks an [`EndpointResolver`] for the endpoints of
//! a remote domain and tries them in order. An endpoint selects a socket
//! only: the remote domain stays the TLS server name and the certificate's
//! reference identity, whatever the endpoint is. This release resolves only
//! explicitly configured endpoints ([`ConfiguredEndpoints`]) and reaches
//! every endpoint with STARTTLS; DNS discovery, with XEP-0368 direct TLS
//! targets, implements the same trait.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use waddle_xmpp::identity::CanonicalDomain;

use crate::s2s::config::S2sConfig;

/// How an endpoint was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointOrigin {
    /// Operator configuration (RFC 6120 §3.2.3); exempt from the address
    /// checks DNS-derived addresses get.
    Configured,
}

/// One address to try with STARTTLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub address: SocketAddr,
    pub origin: EndpointOrigin,
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
    /// Nothing is configured or published for the domain.
    #[error("the remote domain has no endpoint")]
    NoEndpoints,
    /// A lookup failed; this is not the absence of records.
    #[error("the endpoint lookup failed")]
    LookupFailed,
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
            })
            .collect();
        if endpoints.is_empty() {
            return Err(ResolveError::NoEndpoints);
        }
        Ok(Endpoints::new(endpoints))
    }
}
