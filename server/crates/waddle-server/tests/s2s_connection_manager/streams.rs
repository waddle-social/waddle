//! Streams between two in-process deployments: STARTTLS and SASL EXTERNAL
//! in both directions, the evidence a stream carries, refusals before TLS,
//! certificate failures and redirects.

use std::time::Duration;

use rustls::ProtocolVersion;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use waddle_server::s2s::config::AdmissionRefusal;
use waddle_server::s2s::conn::{AuthenticationStrength, ConnectError, StreamDirection};
use waddle_server::s2s::verify::{MatchedIdentifier, TrustProfileKind};
use waddle_xmpp::s2s::StreamErrorCondition;
use waddle_xmpp::telemetry::attributes::S2sOutcome;

use crate::support::{
    domain, eventually, header, listener, pair, raw_exchange, raw_exchange_until, start, Pki,
    Received, Spec, HOME_A, HOME_B,
};

const WAIT: Duration = Duration::from_secs(5);

/// Whether the server has offered STARTTLS or closed the connection.
fn answered(received: &Received) -> bool {
    received.offers_starttls() || received.stream_error().is_some()
}

#[tokio::test(flavor = "multi_thread")]
async fn starttls_and_external_establish_streams_in_both_directions() {
    let pki = Pki::new();
    let (a, b) = pair(&pki, |_| {}).await;

    let outbound = a
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("a stream from home-a to home-b");
    assert_eq!(outbound.direction(), StreamDirection::Outbound);
    assert_eq!(outbound.source(), &domain(HOME_A));
    assert_eq!(outbound.destination(), &domain(HOME_B));
    assert_eq!(outbound.strength(), AuthenticationStrength::Pkix);
    assert_eq!(outbound.trust_profile(), TrustProfileKind::PrivateCa);
    assert_eq!(outbound.matched_identifier(), MatchedIdentifier::DnsId);
    assert!(matches!(
        outbound.tls().version,
        Some(ProtocolVersion::TLSv1_3 | ProtocolVersion::TLSv1_2)
    ));
    assert!(outbound.tls().cipher_suite.is_some());

    // The receiving side holds the matching inbound stream.
    assert!(
        eventually(WAIT, || b.handle.streams().iter().any(|stream| {
            stream.direction() == StreamDirection::Inbound
                && stream.source() == &domain(HOME_A)
                && stream.destination() == &domain(HOME_B)
                && stream.strength() == AuthenticationStrength::Pkix
        }))
        .await
    );

    // Streams are directional: the reverse direction is its own stream.
    let reverse = b
        .handle
        .connect(&domain(HOME_B), &domain(HOME_A))
        .await
        .expect("a stream from home-b to home-a");
    assert_eq!(reverse.source(), &domain(HOME_B));
    assert!(
        eventually(WAIT, || a.handle.streams().iter().any(|stream| {
            stream.direction() == StreamDirection::Inbound && stream.source() == &domain(HOME_B)
        }))
        .await
    );

    // An established stream is reused, not dialled again.
    let again = a
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the existing stream");
    assert_eq!(again.connection(), outbound.connection());

    assert!(a.service.shutdown(WAIT).await.joined);
    assert!(b.service.shutdown(WAIT).await.joined);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_initial_header_without_from_is_refused_before_tls() {
    let pki = Pki::new();
    let (_a, b) = pair(&pki, |_| {}).await;
    let received = raw_exchange(b.address, &header(None, HOME_B), WAIT).await;
    assert!(received.closed, "{received:?}");
    assert_eq!(
        received.stream_error(),
        Some(StreamErrorCondition::BadFormat)
    );
    assert!(!received.offers_starttls(), "no TLS is offered");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admitted_peer_is_offered_starttls() {
    let pki = Pki::new();
    let (_a, b) = pair(&pki, |_| {}).await;
    let received =
        raw_exchange_until(b.address, &header(Some(HOME_A), HOME_B), WAIT, answered).await;
    assert!(received.offers_starttls(), "{received:?}");
    assert_eq!(received.stream_error(), None);
    assert!(!received.closed, "the server awaits STARTTLS");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denylisted_subdomain_is_refused_before_tls_in_open_mode() {
    let pki = Pki::new();
    let (_a, b) = pair(&pki, |spec| {
        spec.mode = "open";
        spec.deny = vec!["evil.test"];
    })
    .await;
    for claimed in ["evil.test", "muc.evil.test"] {
        let received = raw_exchange(b.address, &header(Some(claimed), HOME_B), WAIT).await;
        assert_eq!(
            received.stream_error(),
            Some(StreamErrorCondition::PolicyViolation),
            "{claimed}: {received:?}"
        );
        assert!(!received.offers_starttls(), "{claimed}");
    }
    // A name that merely ends in the same characters is not covered.
    let received = raw_exchange_until(
        b.address,
        &header(Some("badevil.test"), HOME_B),
        WAIT,
        answered,
    )
    .await;
    assert!(received.offers_starttls(), "{received:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ip_addresses_numeric_forms_and_single_label_names_are_refused_before_tls() {
    let pki = Pki::new();
    let (_a, b) = pair(&pki, |spec| spec.mode = "open").await;
    for claimed in ["127.0.0.1", "[::1]", "2130706433", "localhost"] {
        let received = raw_exchange(b.address, &header(Some(claimed), HOME_B), WAIT).await;
        assert_eq!(
            received.stream_error(),
            Some(StreamErrorCondition::PolicyViolation),
            "{claimed}: {received:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unserved_destination_is_host_unknown() {
    let pki = Pki::new();
    let (_a, b) = pair(&pki, |_| {}).await;
    let received = raw_exchange(b.address, &header(Some(HOME_A), "other.test"), WAIT).await;
    assert_eq!(
        received.stream_error(),
        Some(StreamErrorCondition::HostUnknown),
        "{received:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_sent_to_the_starttls_port_is_closed_without_xml() {
    let pki = Pki::new();
    let (_a, b) = pair(&pki, |_| {}).await;
    // The start of a TLS ClientHello record.
    let client_hello = [
        0x16, 0x03, 0x01, 0x00, 0xc0, 0x01, 0x00, 0x00, 0xbc, 0x03, 0x03,
    ];
    let received = raw_exchange(b.address, &client_hello, WAIT).await;
    assert!(received.closed_without_xml(), "{received:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_with_a_certificate_from_another_ca_does_not_authenticate() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let pki = Pki::new();
    let other = Pki::new();
    let (listener_a, address_a) = listener().await;
    let (listener_b, address_b) = listener().await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, address_b)];
    // home-b trusts a CA that did not issue home-a's certificate.
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.endpoints = vec![(HOME_A, address_a)];
    let a = start(&pki, &spec_a, listener_a).await;
    let b_config = spec_b.toml(&pki).replace(
        &pki.ca_path.display().to_string(),
        &other.ca_path.display().to_string(),
    );
    let b_config = b_config.replace(
        &pki.crl_path.display().to_string(),
        &other.crl_path.display().to_string(),
    );
    let b_config = crate::support::load_text(&pki, &b_config)
        .await
        .expect("home-b's configuration");
    let b = waddle_server::s2s::conn::S2sService::start(
        std::sync::Arc::new(b_config),
        Some(listener_b),
        waddle_server::s2s::conn::ServiceOptions::default(),
    );

    let result = a.handle.connect(&domain(HOME_A), &domain(HOME_B)).await;
    // TLS 1.3 completes the client's side of the handshake before the
    // server has checked the client certificate, so home-a sees the alert,
    // or the close that follows it, after the handshake.
    assert!(
        matches!(
            result,
            Err(ConnectError::Failed(
                S2sOutcome::Tls | S2sOutcome::PeerClosed | S2sOutcome::Io
            ))
        ),
        "{result:?}"
    );
    // home-b refused the certificate: an active certificate failure.
    assert!(
        eventually(WAIT, || metrics.counter_sum(
            "waddle.s2s.connections",
            &[("direction", "inbound"), ("outcome", "certificate")]
        ) == Some(1))
        .await
    );
    assert!(b.handle().streams().is_empty());
    assert!(a.handle.streams().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redirect_ends_the_attempt_and_is_never_followed() {
    let pki = Pki::new();
    let redirector = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let redirect_address = redirector.local_addr().expect("address");
    let (listener_a, _) = listener().await;
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_B, "redirect.test"];
    spec.endpoints = vec![("redirect.test", redirect_address)];
    // The backoff outlasts the one second the redirector waits for a
    // second connection below, so the attempt after that wait still finds
    // the domain backing off.
    spec.limits = vec!["backoff_initial_ms = 60000".to_owned()];
    let a = start(&pki, &spec, listener_a).await;

    let server = tokio::spawn(async move {
        let (mut socket, _) = redirector.accept().await.expect("the first connection");
        let mut buffer = [0u8; 1024];
        let _ = socket.read(&mut buffer).await;
        socket
            .write_all(
                b"<?xml version='1.0'?><stream:stream xmlns='jabber:server' \
xmlns:stream='http://etherx.jabber.org/streams' from='redirect.test' to='home-a.test' \
id='r1' version='1.0'><stream:error><see-other-host \
xmlns='urn:ietf:params:xml:ns:xmpp-streams'>127.0.0.1:9</see-other-host></stream:error>\
</stream:stream>",
            )
            .await
            .expect("write the redirect");
        // Waddle must not connect again, here or anywhere else.
        tokio::time::timeout(Duration::from_secs(1), redirector.accept())
            .await
            .is_err()
    });

    let result = a
        .handle
        .connect(&domain(HOME_A), &domain("redirect.test"))
        .await;
    assert_eq!(result, Err(ConnectError::Failed(S2sOutcome::Redirected)));
    assert!(
        server.await.expect("the redirector"),
        "no second connection"
    );
    // The failed attempt backs off.
    assert_eq!(
        a.handle
            .connect(&domain(HOME_A), &domain("redirect.test"))
            .await,
        Err(ConnectError::BackingOff)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn admission_is_checked_before_anything_is_dialled() {
    let pki = Pki::new();
    let (a, _b) = pair(&pki, |_| {}).await;
    assert_eq!(
        a.handle
            .connect(&domain(HOME_A), &domain("other.test"))
            .await,
        Err(ConnectError::NotAdmitted(AdmissionRefusal::NotAllowlisted))
    );
    assert_eq!(
        a.handle
            .connect(&domain(HOME_A), &domain("127.0.0.1"))
            .await,
        Err(ConnectError::NotAdmitted(AdmissionRefusal::NotAPeerDomain))
    );
    assert_eq!(
        a.handle
            .connect(&domain("other.test"), &domain(HOME_B))
            .await,
        Err(ConnectError::NotLocal)
    );
    assert_eq!(a.handle.open_sockets(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_endpoint_fails_and_backs_off() {
    let pki = Pki::new();
    // A port that was bound and released, so nothing listens on it.
    let (unused, closed_address) = listener().await;
    drop(unused);
    let (listener_a, _) = listener().await;
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.endpoints = vec![(HOME_B, closed_address)];
    let a = start(&pki, &spec, listener_a).await;
    assert_eq!(
        a.handle.connect(&domain(HOME_A), &domain(HOME_B)).await,
        Err(ConnectError::Failed(S2sOutcome::Io))
    );
    assert_eq!(
        a.handle.connect(&domain(HOME_A), &domain(HOME_B)).await,
        Err(ConnectError::BackingOff)
    );
}
