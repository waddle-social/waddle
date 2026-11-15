//! DNS discovery of a remote domain's S2S endpoints (ADR-0020 D3; gate
//! G6).
//!
//! To reach domain D, [`DnsDiscovery`] looks up `_xmpps-server._tcp.D.` and
//! `_xmpp-server._tcp.D.` together and orders both record sets as one by
//! priority and weight (XEP-0368 §3, RFC 2782): `_xmpps-server` targets are
//! reached with direct TLS, `_xmpp-server` targets with STARTTLS.
//!
//! - A record set containing a "." target says that service is not offered
//!   for D. The other set may still be used, but A/AAAA fallback is then
//!   forbidden.
//! - The A/AAAA records of D on port 5269 are used only when both lookups
//!   returned a negative answer (NXDOMAIN, or a response without SRV
//!   records). SRV records that exist but name no usable target are not
//!   absence: they forbid the fallback too. A lookup failure (SERVFAIL,
//!   REFUSED, a timeout, a malformed response) is not absence either: if
//!   neither set produced an address the attempt fails with
//!   [`ResolveError::LookupFailed`] and is retried later with backoff,
//!   without fallback. If one set produced addresses and the other lookup
//!   failed, those addresses are used.
//! - A name's A and AAAA lookups are made together; addresses from one are
//!   used when the other fails, and a failure of either with no usable
//!   address from the other is a failed lookup, never absence.
//! - Every name is queried as an absolute [`DnsName`], with a trailing dot,
//!   through a resolver without a search list, so nothing completes a name.
//!   An IP address, numeric IPv4 form or single-label domain, as the
//!   identity profile classifies it, is never resolved; neither is an SRV
//!   target so classified.
//! - At most [`MAX_SRV_TARGETS`] targets are tried per domain and at most
//!   [`MAX_ADDRESSES_PER_TARGET`] addresses per target (or for the
//!   fallback), after addresses in a blocked class or an operator-listed
//!   block ([`BlockedAddresses`]) are removed. The connection manager
//!   checks each DNS-derived address again on the address it connects to.
//! - DNS only selects sockets. The reference identity, the TLS server name
//!   and the direct-TLS Server Name Indication stay D; SRV target names are
//!   never reference identities, and DNS never authenticates a peer.
//!
//! Configured endpoints replace discovery for their domain ([`PeerEndpoints`])
//! and are the only exemption from the address checks. Resolution is behind
//! [`DnsLookup`], so a stub drives the test suite; [`SystemDnsLookup`] uses
//! the system's name servers.

pub mod address;
mod system;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use waddle_xmpp::counter_add;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::telemetry::attributes::S2sDiscovery;

pub use system::{SystemDnsError, SystemDnsLookup};

use address::BlockedAddresses;

use crate::s2s::config::S2sConfig;
use crate::s2s::conn::{
    ConfiguredEndpoints, Endpoint, EndpointOrigin, EndpointResolver, Endpoints, ResolveError,
};
use crate::s2s::verify::OutboundTransport;

/// Most SRV targets tried for one domain, across both record sets.
pub const MAX_SRV_TARGETS: usize = 6;

/// Most addresses tried for one SRV target, or for the A/AAAA fallback.
pub const MAX_ADDRESSES_PER_TARGET: usize = 4;

/// Most records of one SRV answer considered; the rest are ignored.
pub const MAX_SRV_RECORDS: usize = 64;

/// The port of the A/AAAA fallback (RFC 6120 §3.2.2).
pub const FALLBACK_PORT: u16 = 5269;

/// How long one domain's discovery may take in all, every lookup included.
pub const DISCOVERY_DEADLINE: Duration = Duration::from_secs(10);

/// An absolute DNS name in ASCII, ending with a dot, the only form a
/// lookup is made for. The root is `"."`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DnsName(String);

impl DnsName {
    /// Accept an absolute name: ASCII, ending with a dot, no empty label
    /// except the root itself, at most 254 bytes with the final dot.
    pub fn parse(text: &str) -> Option<Self> {
        if text == "." {
            return Some(Self(text.to_owned()));
        }
        let relative = text.strip_suffix('.')?;
        let valid = text.len() <= 254
            && text.is_ascii()
            && relative
                .split('.')
                .all(|label| !label.is_empty() && label.len() <= 63);
        valid.then(|| Self(text.to_owned()))
    }

    /// The absolute name of `domain`.
    pub fn of(domain: &CanonicalDomain) -> Self {
        let mut name = String::with_capacity(domain.as_str().len() + 1);
        name.push_str(domain.as_str());
        name.push('.');
        Self(name)
    }

    /// The SRV owner name of `service` over TCP for `domain` (RFC 2782).
    fn service(service: &str, domain: &CanonicalDomain) -> Self {
        let mut name = String::with_capacity(service.len() + domain.as_str().len() + 7);
        name.push_str(service);
        name.push_str("._tcp.");
        name.push_str(domain.as_str());
        name.push('.');
        Self(name)
    }

    /// The name as queried.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the root, which as an SRV target says the service is
    /// not offered.
    pub fn is_root(&self) -> bool {
        self.0 == "."
    }
}

/// The SRV service of XEP-0368 direct-TLS server-to-server connections.
const XMPPS_SERVER: &str = "_xmpps-server";

/// The SRV service of RFC 6120 server-to-server connections.
const XMPP_SERVER: &str = "_xmpp-server";

/// One SRV record as DNS returned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrvRecord {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    /// The target name; the root says the service is not offered.
    pub target: DnsName,
}

impl SrvRecord {
    fn is_root(&self) -> bool {
        self.target.is_root()
    }
}

/// A lookup's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsAnswer<T> {
    /// Records of the requested type exist. For SRV, a record whose target
    /// cannot be represented as a [`DnsName`] is left out, so the list may
    /// be empty: such records still say the service is published and
    /// forbid the A/AAAA fallback.
    Records(T),
    /// A negative answer: NXDOMAIN, or a successful response without
    /// records of the requested type.
    Negative,
}

/// A lookup that produced no answer: SERVFAIL, REFUSED, a timeout, a
/// malformed response or a local failure. It says nothing about whether
/// records exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DnsFailure;

/// DNS lookups of absolute names. An implementation queries exactly the
/// name it is given and never applies a search list.
#[async_trait]
pub trait DnsLookup: Send + Sync {
    async fn srv(&self, name: &DnsName) -> Result<DnsAnswer<Vec<SrvRecord>>, DnsFailure>;
    async fn ipv4(&self, name: &DnsName) -> Result<DnsAnswer<Vec<Ipv4Addr>>, DnsFailure>;
    async fn ipv6(&self, name: &DnsName) -> Result<DnsAnswer<Vec<Ipv6Addr>>, DnsFailure>;
}

/// Chooses among SRV records of one priority (RFC 2782): given the total
/// weight of the records not yet ordered, it returns a number from zero to
/// that total inclusive.
pub type WeightPicker = Arc<dyn Fn(u32) -> u32 + Send + Sync>;

/// An SRV target with the transport its record set implies.
#[derive(Debug, Clone)]
struct Target {
    record: SrvRecord,
    transport: OutboundTransport,
}

/// What one SRV lookup said.
enum SrvSet {
    Records(Vec<SrvRecord>),
    Negative,
    Failed,
}

/// What resolving one name's addresses found.
struct Addresses {
    /// Addresses outside the blocked classes, IPv6 and IPv4 interleaved,
    /// at most [`MAX_ADDRESSES_PER_TARGET`].
    allowed: Vec<IpAddr>,
    /// Whether DNS returned any address in a blocked class.
    blocked: bool,
    /// Whether either lookup failed.
    failed: bool,
}

/// DNS discovery under ADR-0020 D3.
#[derive(Clone)]
pub struct DnsDiscovery {
    lookup: Arc<dyn DnsLookup>,
    picker: WeightPicker,
    deadline: Duration,
    blocked: Arc<BlockedAddresses>,
}

impl DnsDiscovery {
    /// Discovery through `lookup`, ordering equal priorities at random by
    /// weight, refusing the built-in blocked classes.
    pub fn new(lookup: Arc<dyn DnsLookup>) -> Self {
        Self {
            lookup,
            picker: Arc::new(|total| rand::random_range(0..=total)),
            deadline: DISCOVERY_DEADLINE,
            blocked: Arc::new(BlockedAddresses::default()),
        }
    }

    /// Refuse `blocked`, the built-in classes and the operator's own
    /// blocks, instead of the built-in classes alone.
    pub fn with_blocked_addresses(mut self, blocked: Arc<BlockedAddresses>) -> Self {
        self.blocked = blocked;
        self
    }

    /// Replace the random choice among records of one priority, so a test
    /// can fix the order.
    pub fn with_weight_picker(mut self, picker: WeightPicker) -> Self {
        self.picker = picker;
        self
    }

    /// Replace the bound on one domain's whole discovery.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// The endpoints of `domain`, in the order they are tried.
    pub async fn discover(&self, domain: &CanonicalDomain) -> Result<Endpoints, ResolveError> {
        if !domain.kind().names_federation_peer() {
            return Err(ResolveError::NotAPeerDomain);
        }
        let result = tokio::time::timeout(self.deadline, self.discover_within(domain))
            .await
            .unwrap_or(Err(ResolveError::LookupFailed));
        let source = match &result {
            Ok(endpoints) => match endpoints.iter().next().map(|endpoint| endpoint.origin) {
                Some(EndpointOrigin::Fallback) => S2sDiscovery::Fallback,
                _ => S2sDiscovery::Srv,
            },
            Err(ResolveError::LookupFailed) => S2sDiscovery::LookupFailed,
            Err(ResolveError::Blocked) => S2sDiscovery::Blocked,
            Err(ResolveError::NoEndpoints | ResolveError::NotAPeerDomain) => {
                S2sDiscovery::NoService
            }
        };
        record(source);
        result
    }

    async fn discover_within(&self, domain: &CanonicalDomain) -> Result<Endpoints, ResolveError> {
        let direct_name = DnsName::service(XMPPS_SERVER, domain);
        let starttls_name = DnsName::service(XMPP_SERVER, domain);
        let (direct, starttls) =
            tokio::join!(self.srv_set(&direct_name), self.srv_set(&starttls_name));
        let mut targets = Vec::new();
        let mut published = false;
        let mut failed = false;
        for (set, transport) in [
            (direct, OutboundTransport::DirectTls),
            (starttls, OutboundTransport::StartTls),
        ] {
            match set {
                // Records of either set, "." targets and records without a
                // usable target included, forbid the fallback.
                SrvSet::Records(records) => {
                    published = true;
                    targets.extend(
                        records
                            .into_iter()
                            .filter(|record| !record.is_root())
                            .map(|record| Target { record, transport }),
                    );
                }
                SrvSet::Negative => {}
                SrvSet::Failed => failed = true,
            }
        }
        if !targets.is_empty() {
            return self.srv_endpoints(targets, failed).await;
        }
        if failed {
            return Err(ResolveError::LookupFailed);
        }
        if published {
            return Err(ResolveError::NoEndpoints);
        }
        // No SRV record exists: the domain's own addresses on 5269.
        let addresses = self.addresses(&DnsName::of(domain)).await;
        let endpoints: Vec<Endpoint> = addresses
            .allowed
            .iter()
            .map(|ip| Endpoint {
                address: SocketAddr::new(*ip, FALLBACK_PORT),
                origin: EndpointOrigin::Fallback,
                transport: OutboundTransport::StartTls,
            })
            .collect();
        finish(endpoints, addresses.failed, addresses.blocked)
    }

    async fn srv_set(&self, name: &DnsName) -> SrvSet {
        match self.lookup.srv(name).await {
            Ok(DnsAnswer::Records(mut records)) => {
                records.truncate(MAX_SRV_RECORDS);
                SrvSet::Records(records)
            }
            Ok(DnsAnswer::Negative) => SrvSet::Negative,
            Err(DnsFailure) => SrvSet::Failed,
        }
    }

    /// Order the targets, keep the first [`MAX_SRV_TARGETS`] whose names
    /// are DNS names, and resolve their addresses. `srv_failed` says the
    /// other SRV lookup failed, which makes an attempt without any address
    /// a failed lookup rather than absence.
    async fn srv_endpoints(
        &self,
        targets: Vec<Target>,
        srv_failed: bool,
    ) -> Result<Endpoints, ResolveError> {
        let ordered = order(targets, self.picker.as_ref());
        let chosen: Vec<(Target, CanonicalDomain)> = ordered
            .into_iter()
            .filter_map(|target| target_domain(&target.record).map(|domain| (target, domain)))
            .take(MAX_SRV_TARGETS)
            .collect();
        let names: Vec<DnsName> = chosen
            .iter()
            .map(|(_, domain)| DnsName::of(domain))
            .collect();
        let lookups = names.iter().map(|name| self.addresses(name));
        let resolved = futures::future::join_all(lookups).await;
        let mut endpoints = Vec::new();
        let mut failed = srv_failed;
        let mut blocked = false;
        for ((target, _), addresses) in chosen.iter().zip(resolved) {
            failed |= addresses.failed;
            blocked |= addresses.blocked;
            endpoints.extend(addresses.allowed.iter().map(|ip| Endpoint {
                address: SocketAddr::new(*ip, target.record.port),
                origin: EndpointOrigin::Srv,
                transport: target.transport,
            }));
        }
        finish(endpoints, failed, blocked)
    }

    /// The A and AAAA records of the absolute `name`. Records from either
    /// lookup are used when the other fails; a failure of either is
    /// reported, so a name with no usable address and a failed lookup is
    /// a failed lookup, not absence.
    async fn addresses(&self, name: &DnsName) -> Addresses {
        let (v6, v4) = tokio::join!(self.lookup.ipv6(name), self.lookup.ipv4(name));
        let failed = v6.is_err() || v4.is_err();
        let v6: Vec<IpAddr> = match v6 {
            Ok(DnsAnswer::Records(records)) => records.into_iter().map(IpAddr::V6).collect(),
            _ => Vec::new(),
        };
        let v4: Vec<IpAddr> = match v4 {
            Ok(DnsAnswer::Records(records)) => records.into_iter().map(IpAddr::V4).collect(),
            _ => Vec::new(),
        };
        let refused = |ip: &IpAddr| self.blocked.is_blocked(*ip);
        let blocked = v6.iter().chain(v4.iter()).any(refused);
        let allowed_v6 = v6.into_iter().filter(|ip| !refused(ip));
        let allowed_v4 = v4.into_iter().filter(|ip| !refused(ip));
        Addresses {
            allowed: interleave(allowed_v6, allowed_v4)
                .take(MAX_ADDRESSES_PER_TARGET)
                .collect(),
            blocked,
            failed,
        }
    }
}

/// `first` and `second` alternately, starting with `first`, then the rest
/// of whichever is longer.
fn interleave(
    first: impl Iterator<Item = IpAddr>,
    second: impl Iterator<Item = IpAddr>,
) -> impl Iterator<Item = IpAddr> {
    let mut first = first.fuse();
    let mut second = second.fuse();
    let mut take_first = true;
    std::iter::from_fn(move || {
        let next = if take_first {
            first.next().or_else(|| second.next())
        } else {
            second.next().or_else(|| first.next())
        };
        take_first = !take_first;
        next
    })
}

/// The endpoints found, or why there are none: a failed lookup first,
/// since a retry may find records, then addresses that were all blocked.
fn finish(
    endpoints: Vec<Endpoint>,
    failed: bool,
    blocked: bool,
) -> Result<Endpoints, ResolveError> {
    if !endpoints.is_empty() {
        return Ok(Endpoints::new(endpoints));
    }
    if failed {
        return Err(ResolveError::LookupFailed);
    }
    if blocked {
        return Err(ResolveError::Blocked);
    }
    Err(ResolveError::NoEndpoints)
}

/// An SRV target's name as a canonical DNS name, or `None` when it is an IP
/// address, a numeric IPv4 form, a single-label name or not a domain.
fn target_domain(record: &SrvRecord) -> Option<CanonicalDomain> {
    CanonicalDomain::prepare(record.target.as_str())
        .ok()
        .filter(|domain| domain.kind().names_federation_peer())
}

/// RFC 2782 order: ascending priority; within one priority, records of
/// weight zero first, then a weighted random choice among the records left,
/// repeated until none is left.
fn order(mut targets: Vec<Target>, pick: &(dyn Fn(u32) -> u32 + Send + Sync)) -> Vec<Target> {
    targets.sort_by_key(|target| (target.record.priority, target.record.weight != 0));
    let mut ordered = Vec::with_capacity(targets.len());
    let mut remaining = targets.into_iter().peekable();
    while let Some(first) = remaining.next() {
        let priority = first.record.priority;
        let mut group = vec![first];
        while let Some(next) = remaining.next_if(|next| next.record.priority == priority) {
            group.push(next);
        }
        while !group.is_empty() {
            let total: u32 = group
                .iter()
                .map(|target| u32::from(target.record.weight))
                .sum();
            let chosen = pick(total).min(total);
            let mut running = 0u32;
            let index = group
                .iter()
                .position(|target| {
                    running += u32::from(target.record.weight);
                    running >= chosen
                })
                .unwrap_or(0);
            ordered.push(group.remove(index));
        }
    }
    ordered
}

fn record(source: S2sDiscovery) {
    counter_add!(
        "waddle.s2s.discovery",
        "{lookup}",
        "S2S endpoint resolutions, by where the endpoints came from or why there were none.",
        1,
        source,
    );
}

/// The endpoints of a remote domain: its configured endpoints when it has
/// any (RFC 6120 §3.2.3), otherwise DNS discovery when it is enabled.
#[derive(Clone)]
pub struct PeerEndpoints {
    configured: ConfiguredEndpoints,
    config: Arc<S2sConfig>,
    dns: Option<DnsDiscovery>,
}

impl PeerEndpoints {
    /// Configured endpoints, and `dns` for every other domain when the
    /// configuration enables discovery, refusing the configuration's
    /// blocked addresses.
    pub fn new(config: Arc<S2sConfig>, dns: Option<DnsDiscovery>) -> Self {
        let dns = dns
            .filter(|_| config.discovery_enabled())
            .map(|dns| dns.with_blocked_addresses(Arc::clone(config.blocked_addresses())));
        Self {
            configured: ConfiguredEndpoints::new(Arc::clone(&config)),
            config,
            dns,
        }
    }

    /// The resolver a deployment runs with: configured endpoints, and DNS
    /// discovery through the system's name servers when the configuration
    /// enables it.
    pub fn for_config(config: Arc<S2sConfig>) -> Result<Self, SystemDnsError> {
        let dns = if config.discovery_enabled() {
            Some(DnsDiscovery::new(Arc::new(SystemDnsLookup::from_system()?)))
        } else {
            None
        };
        Ok(Self::new(config, dns))
    }
}

#[async_trait]
impl EndpointResolver for PeerEndpoints {
    async fn resolve(&self, domain: &CanonicalDomain) -> Result<Endpoints, ResolveError> {
        if !self.config.endpoints(domain).is_empty() {
            record(S2sDiscovery::Configured);
            return self.configured.resolve(domain).await;
        }
        match &self.dns {
            Some(dns) => dns.discover(domain).await,
            None => {
                record(S2sDiscovery::NoService);
                Err(ResolveError::NoEndpoints)
            }
        }
    }
}
