//! Configuration of the discovery slice: the peer scope that lifts the
//! `.test` and loopback restriction, discovery, direct-TLS endpoints, the
//! global dial rate and the dialback deadline; and [`PeerEndpoints`],
//! where configured endpoints replace discovery.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use waddle_server::s2s::config::{AdmissionRefusal, ConfigError, ConfigSection, PeerScope};
use waddle_server::s2s::conn::{EndpointOrigin, EndpointResolver, ResolveError};
use waddle_server::s2s::discovery::{DnsDiscovery, DnsLookup, PeerEndpoints};
use waddle_server::s2s::verify::OutboundTransport;

use super::dns::{record, Stub, StubDns};
use crate::support::{domain, Pki, Spec, HOME_A, HOME_B};

fn endpoint_table(domain: &str, address: &str, transport: Option<&str>) -> Vec<String> {
    let mut lines = vec![
        "[[endpoints]]".to_owned(),
        format!("domain = {domain:?}"),
        format!("address = {address:?}"),
    ];
    if let Some(transport) = transport {
        lines.push(format!("transport = {transport:?}"));
    }
    lines
}

#[tokio::test]
async fn the_test_scope_is_the_default_and_refuses_real_domains_and_addresses() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_B, "example.org"];
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::OutsideTestDomain {
            section: ConfigSection::Allowlist,
            index: 1
        })
    ));

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.listen = Some("192.0.2.10:5269".to_owned());
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::NotLoopback(ConfigSection::Listen))
    ));

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.tables = endpoint_table(HOME_B, "192.0.2.10:5269", None);
    assert!(matches!(
        spec.load(&pki).await,
        Err(ConfigError::NotLoopback(ConfigSection::Endpoints))
    ));

    let config = Spec::new(HOME_A, HOME_B)
        .load(&pki)
        .await
        .expect("a test-scope configuration");
    assert_eq!(config.admission().scope(), PeerScope::Test);
    assert!(!config.discovery_enabled());
}

#[tokio::test]
async fn the_public_scope_admits_real_domains_and_any_address() {
    let pki = Pki::new();
    let mut spec = Spec::new("xmpp.example.net", "example.org");
    spec.top = vec!["peer_scope = \"public\"".to_owned()];
    spec.listen = Some("192.0.2.10:5269".to_owned());
    spec.tables = endpoint_table("example.org", "198.51.100.7:5269", None);
    let config = spec.load(&pki).await.expect("a public-scope configuration");
    assert_eq!(config.admission().scope(), PeerScope::Public);
    assert_eq!(
        config.listen(),
        Some(SocketAddr::from((Ipv4Addr::new(192, 0, 2, 10), 5269)))
    );
    let example = domain("example.org");
    assert_eq!(config.admission().decide(&example), Ok(()));
    assert_eq!(
        config.endpoints(&example)[0].address,
        SocketAddr::from((Ipv4Addr::new(198, 51, 100, 7), 5269))
    );
    // Admission is still only what the lists say.
    assert_eq!(
        config.admission().decide(&domain("other.example")),
        Err(AdmissionRefusal::NotAllowlisted)
    );
    // Refused domain forms stay refused in every scope.
    assert_eq!(
        config
            .admission()
            .decide(&waddle_xmpp::identity::CanonicalDomain::prepare("127.1").expect("prepared")),
        Err(AdmissionRefusal::NotAPeerDomain)
    );
}

#[tokio::test]
async fn discovery_and_direct_tls_endpoints_are_configured() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.tables = endpoint_table(HOME_B, "127.0.0.1:5270", Some("direct_tls"));
    spec.tables
        .extend(endpoint_table(HOME_B, "127.0.0.1:5269", Some("starttls")));
    spec.tables
        .extend(["[discovery]".to_owned(), "enabled = true".to_owned()]);
    let config = spec.load(&pki).await.expect("a configuration");
    assert!(config.discovery_enabled());
    let endpoints = config.endpoints(&domain(HOME_B));
    assert_eq!(endpoints.len(), 2);
    assert_eq!(endpoints[0].transport, OutboundTransport::DirectTls);
    assert_eq!(endpoints[1].transport, OutboundTransport::StartTls);

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.tables = endpoint_table(HOME_B, "127.0.0.1:5270", Some("websocket"));
    assert!(matches!(spec.load(&pki).await, Err(ConfigError::Syntax(_))));
}

#[tokio::test]
async fn the_dial_rate_and_dialback_deadline_stay_within_their_ranges() {
    let pki = Pki::new();
    let config = Spec::new(HOME_A, HOME_B)
        .load(&pki)
        .await
        .expect("defaults");
    assert_eq!(config.limits().dial_rate.per_second, 4);
    assert_eq!(config.limits().dial_rate.burst, 8);
    assert_eq!(config.deadlines().dialback, Duration::from_secs(15));

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.limits = vec![
        "dial_per_second = 1".to_owned(),
        "dial_burst = 2".to_owned(),
        "dialback_timeout_ms = 1000".to_owned(),
    ];
    let config = spec.load(&pki).await.expect("lowered limits");
    assert_eq!(config.limits().dial_rate.per_second, 1);
    assert_eq!(config.limits().dial_rate.burst, 2);
    assert_eq!(config.deadlines().dialback, Duration::from_secs(1));

    for (line, name) in [
        ("dial_per_second = 0", "dial_per_second"),
        ("dial_per_second = 5", "dial_per_second"),
        ("dial_burst = 9", "dial_burst"),
        ("dialback_timeout_ms = 0", "dialback_timeout_ms"),
        ("dialback_timeout_ms = 15001", "dialback_timeout_ms"),
    ] {
        let mut spec = Spec::new(HOME_A, HOME_B);
        spec.limits = vec![line.to_owned()];
        match spec.load(&pki).await {
            Err(ConfigError::LimitOutOfRange(refused)) => assert_eq!(refused, name, "{line}"),
            other => panic!("{line}: expected an out-of-range limit, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn configured_endpoints_replace_discovery_for_their_domain() {
    let pki = Pki::new();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_B, "other.test"];
    spec.endpoints = vec![(HOME_B, SocketAddr::from(([127, 0, 0, 1], 5269)))];
    spec.tables = vec!["[discovery]".to_owned(), "enabled = true".to_owned()];
    let config = Arc::new(spec.load(&pki).await.expect("a configuration"));
    let stub = Arc::new(
        StubDns::default()
            .srv(
                "_xmpp-server._tcp.other.test.",
                Stub::Records(vec![record(10, 0, 5269, "xmpp.other.test.")]),
            )
            .a(
                "xmpp.other.test.",
                Stub::Records(vec!["8.8.8.8".parse().expect("v4")]),
            ),
    );
    let resolver = PeerEndpoints::new(
        Arc::clone(&config),
        Some(DnsDiscovery::new(Arc::clone(&stub) as Arc<dyn DnsLookup>)),
    );

    let configured = resolver
        .resolve(&domain(HOME_B))
        .await
        .expect("configured endpoints");
    let configured: Vec<_> = configured.iter().copied().collect();
    assert_eq!(configured.len(), 1);
    assert_eq!(configured[0].origin, EndpointOrigin::Configured);
    assert!(
        stub.queries().is_empty(),
        "a configured domain is never looked up"
    );

    let discovered = resolver
        .resolve(&domain("other.test"))
        .await
        .expect("discovered endpoints");
    let discovered: Vec<_> = discovered.iter().copied().collect();
    assert_eq!(discovered[0].origin, EndpointOrigin::Srv);
    assert!(!stub.queries().is_empty());
}

#[tokio::test]
async fn without_discovery_only_configured_endpoints_exist() {
    let pki = Pki::new();
    let config = Arc::new(
        Spec::new(HOME_A, HOME_B)
            .load(&pki)
            .await
            .expect("a configuration"),
    );
    let stub = Arc::new(StubDns::default());
    let resolver = PeerEndpoints::new(
        config,
        Some(DnsDiscovery::new(Arc::clone(&stub) as Arc<dyn DnsLookup>)),
    );
    assert_eq!(
        resolver.resolve(&domain("other.test")).await.map(|_| ()),
        Err(ResolveError::NoEndpoints)
    );
    assert!(stub.queries().is_empty());
}
