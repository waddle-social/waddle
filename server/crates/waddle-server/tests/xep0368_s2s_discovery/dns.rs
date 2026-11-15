//! `DnsDiscovery` against a stub resolver: record-set merging and order,
//! "." targets, SRV records without a usable target, fallback, failures
//! of either lookup, names queried, refused domain forms, caps, the
//! operator's blocked prefixes and the discovery deadline.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use waddle_server::s2s::conn::{Endpoint, EndpointOrigin, ResolveError};
use waddle_server::s2s::discovery::address::{AddressBlock, BlockedAddresses};
use waddle_server::s2s::discovery::{
    DnsAnswer, DnsDiscovery, DnsFailure, DnsLookup, DnsName, SrvRecord, FALLBACK_PORT,
    MAX_ADDRESSES_PER_TARGET, MAX_SRV_TARGETS,
};
use waddle_server::s2s::verify::OutboundTransport;
use waddle_xmpp::identity::CanonicalDomain;

pub const DOMAIN: &str = "example.org";
pub const DIRECT_SRV: &str = "_xmpps-server._tcp.example.org.";
pub const STARTTLS_SRV: &str = "_xmpp-server._tcp.example.org.";

/// One stub answer.
#[derive(Clone)]
pub enum Stub<T> {
    Records(T),
    Negative,
    Failure,
}

impl<T> Stub<T> {
    fn answer(self) -> Result<DnsAnswer<T>, DnsFailure> {
        match self {
            Self::Records(records) => Ok(DnsAnswer::Records(records)),
            Self::Negative => Ok(DnsAnswer::Negative),
            Self::Failure => Err(DnsFailure),
        }
    }
}

/// A stub resolver: answers by absolute name, negative for every name it
/// does not know, and records every query as `TYPE name`.
#[derive(Default)]
pub struct StubDns {
    srv: HashMap<String, Stub<Vec<SrvRecord>>>,
    a: HashMap<String, Stub<Vec<Ipv4Addr>>>,
    aaaa: HashMap<String, Stub<Vec<Ipv6Addr>>>,
    delay: Option<Duration>,
    queries: Mutex<Vec<String>>,
}

impl StubDns {
    pub fn srv(mut self, name: &str, answer: Stub<Vec<SrvRecord>>) -> Self {
        self.srv.insert(name.to_owned(), answer);
        self
    }

    pub fn a(mut self, name: &str, answer: Stub<Vec<Ipv4Addr>>) -> Self {
        self.a.insert(name.to_owned(), answer);
        self
    }

    pub fn aaaa(mut self, name: &str, answer: Stub<Vec<Ipv6Addr>>) -> Self {
        self.aaaa.insert(name.to_owned(), answer);
        self
    }

    /// Answer every query only after `delay`.
    pub fn slow(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    pub fn queries(&self) -> Vec<String> {
        match self.queries.lock() {
            Ok(queries) => queries.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    async fn note(&self, query: String) {
        match self.queries.lock() {
            Ok(mut queries) => queries.push(query),
            Err(poisoned) => poisoned.into_inner().push(query),
        }
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
    }
}

#[async_trait]
impl DnsLookup for StubDns {
    async fn srv(&self, name: &DnsName) -> Result<DnsAnswer<Vec<SrvRecord>>, DnsFailure> {
        let name = name.as_str();
        self.note(format!("SRV {name}")).await;
        self.srv
            .get(name)
            .cloned()
            .unwrap_or(Stub::Negative)
            .answer()
    }

    async fn ipv4(&self, name: &DnsName) -> Result<DnsAnswer<Vec<Ipv4Addr>>, DnsFailure> {
        let name = name.as_str();
        self.note(format!("A {name}")).await;
        self.a.get(name).cloned().unwrap_or(Stub::Negative).answer()
    }

    async fn ipv6(&self, name: &DnsName) -> Result<DnsAnswer<Vec<Ipv6Addr>>, DnsFailure> {
        let name = name.as_str();
        self.note(format!("AAAA {name}")).await;
        self.aaaa
            .get(name)
            .cloned()
            .unwrap_or(Stub::Negative)
            .answer()
    }
}

pub fn record(priority: u16, weight: u16, port: u16, target: &str) -> SrvRecord {
    SrvRecord {
        priority,
        weight,
        port,
        target: DnsName::parse(target).expect("an absolute DNS name"),
    }
}

pub fn v4(text: &str) -> Ipv4Addr {
    text.parse().expect("an IPv4 address")
}

pub fn v6(text: &str) -> Ipv6Addr {
    text.parse().expect("an IPv6 address")
}

pub fn prepared(text: &str) -> CanonicalDomain {
    CanonicalDomain::prepare(text).expect("a domain the identity profile prepares")
}

/// Discovery over `stub` that always takes the first record of the
/// remaining ones in a priority, so the order is fixed.
pub fn discovery(stub: &Arc<StubDns>) -> DnsDiscovery {
    DnsDiscovery::new(Arc::clone(stub) as Arc<dyn DnsLookup>).with_weight_picker(Arc::new(|_| 0))
}

fn endpoints(result: Result<waddle_server::s2s::conn::Endpoints, ResolveError>) -> Vec<Endpoint> {
    result
        .expect("discovery finds endpoints")
        .iter()
        .copied()
        .collect()
}

fn socket(ip: &str, port: u16) -> SocketAddr {
    SocketAddr::new(ip.parse::<IpAddr>().expect("an address"), port)
}

#[tokio::test]
async fn both_record_sets_merge_by_priority_with_their_transports() {
    let stub = Arc::new(
        StubDns::default()
            .srv(
                DIRECT_SRV,
                Stub::Records(vec![record(20, 0, 5223, "tls.example.org.")]),
            )
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![
                    record(10, 0, 5269, "first.example.org."),
                    record(30, 0, 5270, "last.example.org."),
                ]),
            )
            .a("first.example.org.", Stub::Records(vec![v4("8.8.8.1")]))
            .a("tls.example.org.", Stub::Records(vec![v4("8.8.8.2")]))
            .a("last.example.org.", Stub::Records(vec![v4("8.8.8.3")])),
    );
    let found = endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await);
    assert_eq!(
        found,
        vec![
            Endpoint {
                address: socket("8.8.8.1", 5269),
                origin: EndpointOrigin::Srv,
                transport: OutboundTransport::StartTls,
            },
            Endpoint {
                address: socket("8.8.8.2", 5223),
                origin: EndpointOrigin::Srv,
                transport: OutboundTransport::DirectTls,
            },
            Endpoint {
                address: socket("8.8.8.3", 5270),
                origin: EndpointOrigin::Srv,
                transport: OutboundTransport::StartTls,
            },
        ]
    );
}

#[tokio::test]
async fn only_fully_qualified_names_are_queried() {
    let stub = Arc::new(
        StubDns::default()
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
            )
            .a("xmpp.example.org.", Stub::Records(vec![v4("8.8.8.8")])),
    );
    discovery(&stub)
        .discover(&prepared("Example.ORG."))
        .await
        .expect("endpoints");
    let mut queries = stub.queries();
    queries.sort();
    assert_eq!(
        queries,
        vec![
            "A xmpp.example.org.".to_owned(),
            "AAAA xmpp.example.org.".to_owned(),
            format!("SRV {STARTTLS_SRV}"),
            format!("SRV {DIRECT_SRV}"),
        ]
    );
    assert!(stub.queries().iter().all(|query| query.ends_with('.')));
}

#[tokio::test]
async fn equal_priorities_follow_weight_and_zero_weight_comes_first() {
    let stub = Arc::new(
        StubDns::default()
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![
                    record(10, 5, 5269, "light.example.org."),
                    record(10, 0, 5269, "zero.example.org."),
                    record(10, 10, 5269, "heavy.example.org."),
                ]),
            )
            .a("light.example.org.", Stub::Records(vec![v4("8.8.8.1")]))
            .a("zero.example.org.", Stub::Records(vec![v4("8.8.8.2")]))
            .a("heavy.example.org.", Stub::Records(vec![v4("8.8.8.3")])),
    );
    // Picking zero always takes the first remaining record: weight zero
    // first, then the rest in the order DNS gave them.
    let first = endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await);
    let order: Vec<SocketAddr> = first.iter().map(|endpoint| endpoint.address).collect();
    assert_eq!(
        order,
        vec![
            socket("8.8.8.2", 5269),
            socket("8.8.8.1", 5269),
            socket("8.8.8.3", 5269)
        ]
    );
    // Picking the total takes the record whose running weight reaches it:
    // the heaviest, then the next, then the zero-weight record.
    let heaviest = DnsDiscovery::new(Arc::clone(&stub) as Arc<dyn DnsLookup>)
        .with_weight_picker(Arc::new(|total| total));
    let order: Vec<SocketAddr> = endpoints(heaviest.discover(&prepared(DOMAIN)).await)
        .iter()
        .map(|endpoint| endpoint.address)
        .collect();
    assert_eq!(
        order,
        vec![
            socket("8.8.8.3", 5269),
            socket("8.8.8.1", 5269),
            socket("8.8.8.2", 5269)
        ]
    );
}

#[tokio::test]
async fn a_dot_target_uses_the_other_set_but_forbids_fallback() {
    let stub = Arc::new(
        StubDns::default()
            .srv(DIRECT_SRV, Stub::Records(vec![record(0, 0, 0, ".")]))
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
            )
            .a("xmpp.example.org.", Stub::Records(vec![v4("8.8.8.8")])),
    );
    let found = endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].transport, OutboundTransport::StartTls);

    // "." in one set and a negative answer for the other: no service, and
    // the domain's own addresses are never looked up.
    let stub = Arc::new(
        StubDns::default()
            .srv(DIRECT_SRV, Stub::Records(vec![record(0, 0, 0, ".")]))
            .a("example.org.", Stub::Records(vec![v4("8.8.8.8")])),
    );
    assert_eq!(
        discovery(&stub).discover(&prepared(DOMAIN)).await,
        Err(ResolveError::NoEndpoints)
    );
    assert!(!stub
        .queries()
        .iter()
        .any(|query| query.contains(" example.org.")));

    // "." in both sets.
    let stub = Arc::new(
        StubDns::default()
            .srv(DIRECT_SRV, Stub::Records(vec![record(0, 0, 0, ".")]))
            .srv(STARTTLS_SRV, Stub::Records(vec![record(0, 0, 0, ".")]))
            .a("example.org.", Stub::Records(vec![v4("8.8.8.8")])),
    );
    assert_eq!(
        discovery(&stub).discover(&prepared(DOMAIN)).await,
        Err(ResolveError::NoEndpoints)
    );
    assert_eq!(stub.queries().len(), 2, "only the two SRV lookups");
}

#[tokio::test]
async fn fallback_to_port_5269_only_after_two_negative_answers() {
    let stub = Arc::new(
        StubDns::default()
            .aaaa("example.org.", Stub::Records(vec![v6("2a00:1450::1")]))
            .a("example.org.", Stub::Records(vec![v4("8.8.8.8")])),
    );
    let found = endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await);
    assert_eq!(
        found,
        vec![
            Endpoint {
                address: socket("2a00:1450::1", FALLBACK_PORT),
                origin: EndpointOrigin::Fallback,
                transport: OutboundTransport::StartTls,
            },
            Endpoint {
                address: socket("8.8.8.8", FALLBACK_PORT),
                origin: EndpointOrigin::Fallback,
                transport: OutboundTransport::StartTls,
            },
        ]
    );
}

#[tokio::test]
async fn a_failed_lookup_is_not_absence_and_never_falls_back() {
    for (direct, starttls) in [
        (Stub::Failure, Stub::Negative),
        (Stub::Negative, Stub::Failure),
        (Stub::Failure, Stub::Failure),
    ] {
        let stub = Arc::new(
            StubDns::default()
                .srv(DIRECT_SRV, direct)
                .srv(STARTTLS_SRV, starttls)
                .a("example.org.", Stub::Records(vec![v4("8.8.8.8")])),
        );
        assert_eq!(
            discovery(&stub).discover(&prepared(DOMAIN)).await,
            Err(ResolveError::LookupFailed)
        );
        assert!(
            stub.queries().iter().all(|query| query.starts_with("SRV ")),
            "no address lookup after a failed SRV lookup"
        );
    }
}

#[tokio::test]
async fn records_from_one_set_are_used_when_the_other_lookup_fails() {
    let stub = Arc::new(
        StubDns::default()
            .srv(DIRECT_SRV, Stub::Failure)
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
            )
            .a("xmpp.example.org.", Stub::Records(vec![v4("8.8.8.8")])),
    );
    let found = endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].address, socket("8.8.8.8", 5269));
}

#[tokio::test]
async fn refused_domain_forms_are_never_resolved() {
    for text in [
        "192.0.2.1",
        "[2001:db8::1]",
        "2130706433",
        "0x7f.1",
        "127.1",
        "localhost",
    ] {
        let stub = Arc::new(StubDns::default());
        assert_eq!(
            discovery(&stub).discover(&prepared(text)).await,
            Err(ResolveError::NotAPeerDomain),
            "{text}"
        );
        assert!(stub.queries().is_empty(), "{text} was looked up");
    }
}

#[tokio::test]
async fn srv_targets_in_refused_forms_are_skipped() {
    let stub = Arc::new(
        StubDns::default()
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![
                    record(1, 0, 5269, "127.0.0.1."),
                    record(2, 0, 5269, "localhost."),
                    record(3, 0, 5269, "2130706433."),
                    record(4, 0, 5269, "xmpp.example.org."),
                ]),
            )
            .a("127.0.0.1.", Stub::Records(vec![v4("8.8.8.1")]))
            .a("localhost.", Stub::Records(vec![v4("8.8.8.2")]))
            .a("xmpp.example.org.", Stub::Records(vec![v4("8.8.8.3")])),
    );
    let found = endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].address, socket("8.8.8.3", 5269));
    let queries = stub.queries();
    for refused in ["127.0.0.1.", "localhost.", "2130706433."] {
        assert!(
            !queries
                .iter()
                .any(|query| query.ends_with(&format!(" {refused}"))),
            "{refused} was looked up"
        );
    }
}

#[tokio::test]
async fn targets_per_domain_and_addresses_per_target_are_capped() {
    let mut stub = StubDns::default();
    let mut records = Vec::new();
    for index in 0..MAX_SRV_TARGETS + 2 {
        let target = format!("t{index}.example.org.");
        records.push(record(
            10 + u16::try_from(index).expect("small"),
            0,
            5269,
            &target,
        ));
        stub = stub.a(
            &target,
            Stub::Records(vec![Ipv4Addr::new(
                8,
                8,
                4,
                u8::try_from(index).expect("small"),
            )]),
        );
    }
    let stub = Arc::new(stub.srv(STARTTLS_SRV, Stub::Records(records)));
    let found = endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await);
    assert_eq!(found.len(), MAX_SRV_TARGETS);
    let queries = stub.queries();
    for index in MAX_SRV_TARGETS..MAX_SRV_TARGETS + 2 {
        let target = format!("t{index}.example.org.");
        assert!(!queries.iter().any(|query| query.ends_with(&target)));
    }

    // Addresses: IPv6 and IPv4 interleaved, at most the cap.
    let stub = Arc::new(
        StubDns::default()
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
            )
            .aaaa(
                "xmpp.example.org.",
                Stub::Records(vec![
                    v6("2a00:1450::1"),
                    v6("2a00:1450::2"),
                    v6("2a00:1450::3"),
                ]),
            )
            .a(
                "xmpp.example.org.",
                Stub::Records(vec![v4("8.8.8.1"), v4("8.8.8.2"), v4("8.8.8.3")]),
            ),
    );
    let found = endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await);
    assert_eq!(found.len(), MAX_ADDRESSES_PER_TARGET);
    let addresses: Vec<SocketAddr> = found.iter().map(|endpoint| endpoint.address).collect();
    assert_eq!(
        addresses,
        vec![
            socket("2a00:1450::1", 5269),
            socket("8.8.8.1", 5269),
            socket("2a00:1450::2", 5269),
            socket("8.8.8.2", 5269),
        ]
    );
}

#[tokio::test]
async fn blocked_addresses_are_removed_and_all_blocked_is_refused() {
    let stub = Arc::new(
        StubDns::default()
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
            )
            .aaaa(
                "xmpp.example.org.",
                Stub::Records(vec![v6("::1"), v6("fd00::1")]),
            )
            .a(
                "xmpp.example.org.",
                Stub::Records(vec![v4("10.0.0.1"), v4("8.8.8.8"), v4("127.0.0.1")]),
            ),
    );
    let found = endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].address, socket("8.8.8.8", 5269));

    let stub = Arc::new(
        StubDns::default()
            .aaaa("example.org.", Stub::Records(vec![v6("::ffff:8.8.8.8")]))
            .a(
                "example.org.",
                Stub::Records(vec![v4("192.168.1.1"), v4("169.254.1.1")]),
            ),
    );
    assert_eq!(
        discovery(&stub).discover(&prepared(DOMAIN)).await,
        Err(ResolveError::Blocked)
    );
}

#[tokio::test]
async fn targets_without_addresses_leave_no_endpoint() {
    // Every target answers negatively: no endpoint and no failure.
    let stub = Arc::new(StubDns::default().srv(
        STARTTLS_SRV,
        Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
    ));
    assert_eq!(
        discovery(&stub).discover(&prepared(DOMAIN)).await,
        Err(ResolveError::NoEndpoints)
    );
    // Both address lookups of the only target fail: retried later.
    let stub = Arc::new(
        StubDns::default()
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
            )
            .a("xmpp.example.org.", Stub::Failure)
            .aaaa("xmpp.example.org.", Stub::Failure),
    );
    assert_eq!(
        discovery(&stub).discover(&prepared(DOMAIN)).await,
        Err(ResolveError::LookupFailed)
    );
    // One address lookup fails, the other answers: its records are used.
    let stub = Arc::new(
        StubDns::default()
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
            )
            .aaaa("xmpp.example.org.", Stub::Failure)
            .a("xmpp.example.org.", Stub::Records(vec![v4("8.8.8.8")])),
    );
    assert_eq!(
        endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await).len(),
        1
    );
}

#[tokio::test]
async fn discovery_that_outlasts_its_deadline_is_a_failed_lookup() {
    let stub = Arc::new(StubDns::default().slow(Duration::from_millis(500)));
    let discovery = discovery(&stub).with_deadline(Duration::from_millis(50));
    assert_eq!(
        discovery.discover(&prepared(DOMAIN)).await,
        Err(ResolveError::LookupFailed)
    );
}

#[tokio::test]
async fn srv_records_without_a_usable_target_forbid_fallback() {
    // SRV records exist, but none has a target this resolver could
    // represent: the service is published, so the domain's own addresses
    // are never used.
    let stub = Arc::new(
        StubDns::default()
            .srv(STARTTLS_SRV, Stub::Records(Vec::new()))
            .a("example.org.", Stub::Records(vec![v4("8.8.8.8")])),
    );
    assert_eq!(
        discovery(&stub).discover(&prepared(DOMAIN)).await,
        Err(ResolveError::NoEndpoints)
    );
    assert!(
        stub.queries().iter().all(|query| query.starts_with("SRV ")),
        "no address lookup after published SRV records"
    );
}

#[tokio::test]
async fn one_failed_address_lookup_without_addresses_is_a_failed_lookup() {
    // An SRV target whose A lookup fails while its AAAA lookup is
    // negative: retried later, never read as absence.
    for (a, aaaa) in [
        (Stub::Failure, Stub::Negative),
        (Stub::Negative, Stub::Failure),
    ] {
        let stub = Arc::new(
            StubDns::default()
                .srv(
                    STARTTLS_SRV,
                    Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
                )
                .a("xmpp.example.org.", a.clone())
                .aaaa("xmpp.example.org.", aaaa.clone()),
        );
        assert_eq!(
            discovery(&stub).discover(&prepared(DOMAIN)).await,
            Err(ResolveError::LookupFailed)
        );
        // The same for the domain's own addresses after two negative SRV
        // answers.
        let stub = Arc::new(
            StubDns::default()
                .a("example.org.", a)
                .aaaa("example.org.", aaaa),
        );
        assert_eq!(
            discovery(&stub).discover(&prepared(DOMAIN)).await,
            Err(ResolveError::LookupFailed)
        );
    }
}

#[tokio::test]
async fn a_failed_srv_lookup_beside_targets_without_addresses_is_a_failed_lookup() {
    let stub = Arc::new(StubDns::default().srv(DIRECT_SRV, Stub::Failure).srv(
        STARTTLS_SRV,
        Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
    ));
    assert_eq!(
        discovery(&stub).discover(&prepared(DOMAIN)).await,
        Err(ResolveError::LookupFailed)
    );
}

#[tokio::test]
async fn operator_blocked_prefixes_are_refused_with_the_built_in_classes() {
    let blocked = BlockedAddresses::new(vec![
        // A network-specific NAT64 prefix (RFC 6052) and an IPv4 block.
        AddressBlock::parse("2a00:64::/96").expect("a prefix"),
        AddressBlock::parse("8.8.4.0/24").expect("a prefix"),
    ]);
    let stub = Arc::new(
        StubDns::default()
            .srv(
                STARTTLS_SRV,
                Stub::Records(vec![record(10, 0, 5269, "xmpp.example.org.")]),
            )
            .aaaa(
                "xmpp.example.org.",
                Stub::Records(vec![v6("2a00:64::808:808"), v6("2002:808:404::1")]),
            )
            .a(
                "xmpp.example.org.",
                Stub::Records(vec![v4("8.8.4.4"), v4("10.0.0.1"), v4("8.8.8.8")]),
            ),
    );
    let found = endpoints(
        discovery(&stub)
            .with_blocked_addresses(Arc::new(blocked))
            .discover(&prepared(DOMAIN))
            .await,
    );
    // The NAT64 address, the 6to4 address embedding 8.8.4.4, 8.8.4.4 and
    // the private address are refused.
    let addresses: Vec<SocketAddr> = found.iter().map(|endpoint| endpoint.address).collect();
    assert_eq!(addresses, vec![socket("8.8.8.8", 5269)]);

    // Without the operator's blocks only the built-in classes apply.
    let found = endpoints(discovery(&stub).discover(&prepared(DOMAIN)).await);
    assert_eq!(found.len(), MAX_ADDRESSES_PER_TARGET);
}
