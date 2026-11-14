//! [`DnsLookup`] through the system's name servers, with Hickory DNS.
//!
//! The name servers and their options come from the system configuration
//! (`/etc/resolv.conf` on Unix); its search list and default domain are
//! dropped, the hosts file is never consulted, and every query is made for
//! the absolute name it is given. Hickory is already in the dependency
//! graph at this version; this module uses its Tokio runtime and system
//! configuration features only.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use async_trait::async_trait;
use hickory_resolver::config::{ResolveHosts, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::net::{DnsError, NetError};
use hickory_resolver::proto::op::ResponseCode;
use hickory_resolver::proto::rr::{Name, RData, RecordType};
use hickory_resolver::{Resolver, TokioResolver};
use thiserror::Error;

use super::{DnsAnswer, DnsFailure, DnsLookup, DnsName, SrvRecord};

/// How long one query waits for a name server.
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);

/// How many times a query is sent to the name servers before it fails.
const QUERY_ATTEMPTS: usize = 2;

/// The system resolver configuration could not be used.
#[derive(Debug, Error)]
pub enum SystemDnsError {
    #[error("the system DNS configuration cannot be read")]
    Configuration,
    #[error("the system DNS configuration names no name server")]
    NoNameServers,
    #[error("the DNS resolver cannot be built")]
    Resolver,
}

/// DNS lookups through the system's name servers.
#[derive(Clone)]
pub struct SystemDnsLookup {
    resolver: TokioResolver,
}

impl SystemDnsLookup {
    /// A resolver for the system's name servers, without a search list.
    pub fn from_system() -> Result<Self, SystemDnsError> {
        let (system, mut options) = hickory_resolver::system_conf::read_system_conf()
            .map_err(|_| SystemDnsError::Configuration)?;
        if system.name_servers().is_empty() {
            return Err(SystemDnsError::NoNameServers);
        }
        // No default domain and no search list: nothing can complete a
        // name (ADR-0020 D3).
        let config = ResolverConfig::from_parts(None, Vec::new(), system.name_servers().to_vec());
        options.use_hosts_file = ResolveHosts::Never;
        options.timeout = QUERY_TIMEOUT;
        options.attempts = QUERY_ATTEMPTS;
        let resolver = Resolver::builder_with_config(config, TokioRuntimeProvider::default())
            .with_options(options)
            .build()
            .map_err(|_| SystemDnsError::Resolver)?;
        Ok(Self { resolver })
    }

    async fn records(&self, name: &DnsName, kind: RecordType) -> Result<Vec<RData>, DnsFailure> {
        let name = Name::from_ascii(name.as_str()).map_err(|_| DnsFailure)?;
        // An absolute name is queried as it is, never with a search suffix.
        if !name.is_fqdn() {
            return Err(DnsFailure);
        }
        match self.resolver.lookup(name, kind).await {
            Ok(lookup) => Ok(lookup
                .answers()
                .iter()
                .map(|record| record.data.clone())
                .collect()),
            Err(error) if is_negative(&error) => Ok(Vec::new()),
            Err(_) => Err(DnsFailure),
        }
    }
}

/// Whether `error` is a negative answer: NXDOMAIN, or a successful
/// response without records of the requested type. Every other error is a
/// failure.
fn is_negative(error: &NetError) -> bool {
    match error {
        NetError::Dns(DnsError::NoRecordsFound(no_records)) => matches!(
            no_records.response_code,
            ResponseCode::NXDomain | ResponseCode::NoError
        ),
        _ => false,
    }
}

fn answer<T>(records: Vec<T>) -> DnsAnswer<Vec<T>> {
    if records.is_empty() {
        DnsAnswer::Negative
    } else {
        DnsAnswer::Records(records)
    }
}

#[async_trait]
impl DnsLookup for SystemDnsLookup {
    async fn srv(&self, name: &DnsName) -> Result<DnsAnswer<Vec<SrvRecord>>, DnsFailure> {
        let records = self.records(name, RecordType::SRV).await?;
        Ok(answer(
            records
                .into_iter()
                .filter_map(|data| match data {
                    RData::SRV(srv) => {
                        // A target that is not an absolute ASCII name is
                        // dropped with its record.
                        let target = if srv.target.is_root() {
                            DnsName::parse(".")
                        } else {
                            DnsName::parse(&srv.target.to_ascii())
                        }?;
                        Some(SrvRecord {
                            priority: srv.priority,
                            weight: srv.weight,
                            port: srv.port,
                            target,
                        })
                    }
                    _ => None,
                })
                .collect(),
        ))
    }

    async fn ipv4(&self, name: &DnsName) -> Result<DnsAnswer<Vec<Ipv4Addr>>, DnsFailure> {
        let records = self.records(name, RecordType::A).await?;
        Ok(answer(
            records
                .into_iter()
                .filter_map(|data| match data {
                    RData::A(address) => Some(address.0),
                    _ => None,
                })
                .collect(),
        ))
    }

    async fn ipv6(&self, name: &DnsName) -> Result<DnsAnswer<Vec<Ipv6Addr>>, DnsFailure> {
        let records = self.records(name, RecordType::AAAA).await?;
        Ok(answer(
            records
                .into_iter()
                .filter_map(|data| match data {
                    RData::AAAA(address) => Some(address.0),
                    _ => None,
                })
                .collect(),
        ))
    }
}
