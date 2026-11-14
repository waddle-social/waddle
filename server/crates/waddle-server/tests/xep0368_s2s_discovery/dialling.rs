//! The dialler with discovery's endpoints, on real loopback sockets:
//! XEP-0368 direct TLS with its Server Name Indication and ALPN protocol,
//! DNS-derived addresses checked on the address connected to, the global
//! dial rate, and backoff after a failed lookup.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use minidom::Element;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::LazyConfigAcceptor;
use waddle_server::s2s::conn::{
    ConnectError, Endpoint, EndpointOrigin, EndpointResolver, Endpoints, ResolveError,
    ServiceOptions,
};
use waddle_server::s2s::verify::{OutboundTransport, DIRECT_TLS_ALPN};
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::ns::{SASL, STREAM};
use waddle_xmpp::s2s::{
    CodecLimits, HeaderAddress, StreamDecoder, StreamEncoder, StreamHeader, StreamLanguage,
};
use waddle_xmpp::telemetry::attributes::S2sOutcome;

use crate::support::{
    await_element, await_header, dead_port, domain, listener, start, start_with, Pki, Spec, HOME_A,
    HOME_B,
};

const WAIT: Duration = Duration::from_secs(5);

/// A resolver that answers every domain with what a test sets.
struct FixedResolver {
    answer: Mutex<Result<Vec<Endpoint>, ResolveError>>,
}

impl FixedResolver {
    fn new(answer: Result<Vec<Endpoint>, ResolveError>) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(answer),
        })
    }

    fn set(&self, answer: Result<Vec<Endpoint>, ResolveError>) {
        match self.answer.lock() {
            Ok(mut current) => *current = answer,
            Err(poisoned) => *poisoned.into_inner() = answer,
        }
    }
}

#[async_trait]
impl EndpointResolver for FixedResolver {
    async fn resolve(&self, _domain: &CanonicalDomain) -> Result<Endpoints, ResolveError> {
        let answer = match self.answer.lock() {
            Ok(answer) => answer.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        answer.map(Endpoints::new)
    }
}

fn options(resolver: &Arc<FixedResolver>) -> ServiceOptions {
    ServiceOptions {
        resolver: Some(Arc::clone(resolver) as Arc<dyn EndpointResolver>),
        ..Default::default()
    }
}

fn starttls_endpoint(address: SocketAddr, origin: EndpointOrigin) -> Endpoint {
    Endpoint {
        address,
        origin,
        transport: OutboundTransport::StartTls,
    }
}

/// `<stream:features>` offering SASL EXTERNAL.
fn external_features() -> Element {
    let mechanism = Element::builder("mechanism", SASL)
        .append("EXTERNAL")
        .build();
    let mechanisms = Element::builder("mechanisms", SASL)
        .append(mechanism)
        .build();
    Element::builder("features", STREAM)
        .append(mechanisms)
        .build()
}

/// What the hand-written direct-TLS receiving server saw.
struct DirectTlsObservation {
    server_name: Option<String>,
    alpn: Vec<Vec<u8>>,
    first_header: StreamHeader,
}

/// A receiving server for home-b.test on `listener` that accepts one
/// direct-TLS connection, records the ClientHello and completes SASL
/// EXTERNAL without checking anything. It keeps the connection open until
/// `release` fires.
async fn direct_tls_server(
    pki: &Pki,
    listener: TcpListener,
    release: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<DirectTlsObservation> {
    let config = Arc::new(pki.server_config(HOME_B, &[DIRECT_TLS_ALPN]));
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("an inbound connection");
        let start = LazyConfigAcceptor::new(rustls::server::Acceptor::default(), socket)
            .await
            .expect("a TLS ClientHello as the first bytes");
        let hello = start.client_hello();
        let server_name = hello.server_name().map(str::to_owned);
        let alpn = hello
            .alpn()
            .map(|protocols| protocols.map(<[u8]>::to_vec).collect())
            .unwrap_or_default();
        let mut tls = start.into_stream(config).await.expect("the TLS handshake");

        let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
        let first_header = await_header(&mut tls, &mut decoder).await;
        let response = StreamHeader::response(
            domain(HOME_B),
            Some(domain(HOME_A)),
            StreamLanguage::english(),
        );
        let (mut encoder, bytes) = StreamEncoder::open(&response).expect("a header");
        tls.write_all(&bytes).await.expect("write a header");
        let features = encoder.element(&external_features()).expect("features");
        tls.write_all(&features).await.expect("write features");
        await_element(&mut tls, &mut decoder, "auth", SASL).await;
        let success = encoder
            .element(&Element::builder("success", SASL).build())
            .expect("success");
        tls.write_all(&success).await.expect("write success");

        let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
        await_header(&mut tls, &mut decoder).await;
        let response = StreamHeader::response(
            domain(HOME_B),
            Some(domain(HOME_A)),
            StreamLanguage::english(),
        );
        let (mut encoder, bytes) = StreamEncoder::open(&response).expect("a header");
        tls.write_all(&bytes).await.expect("write a header");
        let features = encoder
            .element(&Element::builder("features", STREAM).build())
            .expect("features");
        tls.write_all(&features).await.expect("write features");
        tls.flush().await.expect("flush");
        let _ = release.await;
        DirectTlsObservation {
            server_name,
            alpn,
            first_header,
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_direct_tls_endpoint_is_reached_with_sni_and_alpn_and_no_starttls() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let pki = Pki::new();
    let (peer_listener, peer_address) = listener().await;
    let (release, released) = tokio::sync::oneshot::channel();
    let server = direct_tls_server(&pki, peer_listener, released).await;

    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.tables = vec![
        "[[endpoints]]".to_owned(),
        format!("domain = {HOME_B:?}"),
        format!("address = \"{peer_address}\""),
        "transport = \"direct_tls\"".to_owned(),
    ];
    let (listener_a, _) = listener().await;
    let a = start(&pki, &spec, listener_a).await;
    let stream = tokio::time::timeout(WAIT, a.handle.connect(&domain(HOME_A), &domain(HOME_B)))
        .await
        .expect("the attempt ends in time")
        .expect("a direct-TLS stream");
    assert_eq!(stream.remote(), &domain(HOME_B));

    let _ = release.send(());
    let observed = server.await.expect("the server task");
    // XEP-0368: SNI is the domain being reached and ALPN is xmpp-server.
    assert_eq!(observed.server_name.as_deref(), Some(HOME_B));
    assert_eq!(observed.alpn, vec![DIRECT_TLS_ALPN.to_vec()]);
    // The first thing inside TLS is the stream header; nothing preceded
    // TLS, so no plaintext header or `<starttls/>` was ever sent.
    assert_eq!(
        observed.first_header.from,
        HeaderAddress::Domain(domain(HOME_A))
    );
    assert_eq!(
        observed.first_header.to,
        HeaderAddress::Domain(domain(HOME_B))
    );
    assert_eq!(
        metrics.counter_sum(
            "waddle.s2s.established",
            &[("direction", "outbound"), ("transport", "direct_tls")],
        ),
        Some(1)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dns_derived_address_in_a_blocked_class_is_never_dialled() {
    let pki = Pki::new();
    // A live listener on loopback: what DNS would point at.
    let target = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let target_address = target.local_addr().expect("an address");
    let resolver = FixedResolver::new(Ok(vec![starttls_endpoint(
        target_address,
        EndpointOrigin::Srv,
    )]));
    let spec = Spec::new(HOME_A, HOME_B);
    let (listener_a, _) = listener().await;
    let a = start_with(&pki, &spec, listener_a, options(&resolver)).await;

    assert_eq!(
        a.handle.connect(&domain(HOME_A), &domain(HOME_B)).await,
        Err(ConnectError::Failed(S2sOutcome::Io))
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), target.accept())
            .await
            .is_err(),
        "a blocked SRV address was dialled"
    );

    // The same loopback address as a fallback address is refused too.
    let b_spec = Spec::new(HOME_A, "other.test");
    let resolver = FixedResolver::new(Ok(vec![starttls_endpoint(
        target_address,
        EndpointOrigin::Fallback,
    )]));
    let (listener_c, _) = listener().await;
    let c = start_with(&pki, &b_spec, listener_c, options(&resolver)).await;
    assert_eq!(
        c.handle
            .connect(&domain(HOME_A), &domain("other.test"))
            .await,
        Err(ConnectError::Failed(S2sOutcome::Io))
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), target.accept())
            .await
            .is_err(),
        "a blocked fallback address was dialled"
    );

    // A configured endpoint is exempt and is dialled.
    resolver.set(Ok(vec![starttls_endpoint(
        target_address,
        EndpointOrigin::Configured,
    )]));
    let handle = c.handle.clone();
    let attempt =
        tokio::spawn(async move { handle.connect(&domain(HOME_A), &domain("other.test")).await });
    let accepted = tokio::time::timeout(WAIT, target.accept()).await;
    assert!(
        matches!(accepted, Ok(Ok(_))),
        "a configured endpoint was not dialled"
    );
    drop(accepted);
    let _ = tokio::time::timeout(WAIT, attempt).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_global_dial_rate_refuses_attempts_beyond_its_burst() {
    let pki = Pki::new();
    let (_held_one, dead_one) = dead_port();
    let (_held_two, dead_two) = dead_port();
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_B, "dead-1.test", "dead-2.test"];
    spec.endpoints = vec![("dead-1.test", dead_one), ("dead-2.test", dead_two)];
    spec.limits = vec![
        "dial_per_second = 1".to_owned(),
        "dial_burst = 1".to_owned(),
    ];
    let (listener_a, _) = listener().await;
    let a = start(&pki, &spec, listener_a).await;
    let home_a = domain(HOME_A);

    assert_eq!(
        a.handle.connect(&home_a, &domain("dead-1.test")).await,
        Err(ConnectError::Failed(S2sOutcome::Io))
    );
    // Another domain without a backoff entry, at once: the rate refuses
    // it, whatever the backoff table says.
    assert_eq!(
        a.handle.connect(&home_a, &domain("dead-2.test")).await,
        Err(ConnectError::RateLimited)
    );
    // A token accrues after a second.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        a.handle.connect(&home_a, &domain("dead-2.test")).await,
        Err(ConnectError::Failed(S2sOutcome::Io))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_lookup_backs_the_domain_off_without_fallback() {
    let pki = Pki::new();
    let resolver = FixedResolver::new(Err(ResolveError::LookupFailed));
    let spec = Spec::new(HOME_A, HOME_B);
    let (listener_a, _) = listener().await;
    let a = start_with(&pki, &spec, listener_a, options(&resolver)).await;
    let (home_a, home_b) = (domain(HOME_A), domain(HOME_B));
    assert_eq!(
        a.handle.connect(&home_a, &home_b).await,
        Err(ConnectError::Resolve(ResolveError::LookupFailed))
    );
    assert_eq!(
        a.handle.connect(&home_a, &home_b).await,
        Err(ConnectError::BackingOff)
    );

    // Absence is not a failure: no backoff.
    let resolver = FixedResolver::new(Err(ResolveError::NoEndpoints));
    let (listener_c, _) = listener().await;
    let c = start_with(&pki, &spec, listener_c, options(&resolver)).await;
    for _ in 0..2 {
        assert_eq!(
            c.handle.connect(&home_a, &home_b).await,
            Err(ConnectError::Resolve(ResolveError::NoEndpoints))
        );
    }
}
