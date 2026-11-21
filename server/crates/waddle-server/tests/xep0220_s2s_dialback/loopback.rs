//! Dialback between in-process deployments on loopback, with hand-written
//! peers for the side a deployment cannot play: a peer without a
//! certificate that authenticates to home-b.test by dialback while the real
//! home-a.test answers home-b's verification request as the Authoritative
//! Server, and a receiving server for home-b.test that offers no SASL
//! EXTERNAL, or offers it and refuses it as a server that cannot verify
//! home-a's certificate does, so that home-a.test authenticates itself by
//! dialback. The
//! verification connection's outcomes are covered too: an Authoritative
//! Server whose certificate fails, a DNS-derived address in a blocked
//! class, and the verification budget; and a `dialback` stream never
//! replaces a `pkix` one.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use minidom::rxml::xml_ncname;
use minidom::Element;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use waddle_server::s2s::conn::{
    AuthenticationStrength, Endpoint, EndpointOrigin, EndpointResolver, Endpoints, ResolveError,
    ServiceOptions, StreamDirection, VerifiedPeerStream,
};
use waddle_server::s2s::dialback::DialbackSecret;
use waddle_server::s2s::verify::{OutboundTransport, TrustProfileKind};
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::ns::{DIALBACK_FEATURES, JABBER_SERVER_DIALBACK, SASL, STANZAS, STREAM, TLS};
use waddle_xmpp::s2s::{
    CodecLimits, DialbackKeys, HeaderAddress, Inbound, StreamDecoder, StreamEncoder, StreamHeader,
    StreamId, StreamLanguage, StreamVersion,
};

use crate::support::{
    await_element, await_header, domain, eventually, listener, start, start_with, Deployment, Pki,
    Spec, HOME_A, HOME_B,
};

const WAIT: Duration = Duration::from_secs(10);

/// The secret home-a.test's replicas share.
const SECRET: &str = "home-a's dialback secret, shared by its replicas";

fn secret_key(receiving: &str, originating: &str, stream_id: &StreamId) -> String {
    DialbackSecret::new(SECRET.as_bytes())
        .key(&domain(receiving), &domain(originating), stream_id)
        .expect("a key")
        .as_str()
        .to_owned()
}

/// `[dialback]` with the Initiating and Authoritative roles.
fn outbound_dialback(pki: &Pki) -> Vec<String> {
    let path = pki.file("home-a.dialback.secret", SECRET);
    vec![
        "[dialback]".to_owned(),
        "outbound = true".to_owned(),
        format!("secret_file = {:?}", path.display().to_string()),
    ]
}

/// `[dialback]` with the Receiving role.
fn inbound_dialback() -> Vec<String> {
    vec!["[dialback]".to_owned(), "inbound = true".to_owned()]
}

/// home-a.test as the Authoritative Server, and home-b.test, which accepts
/// dialback from home-a.test when `listed` and verifies keys with home-a.
async fn deployments(pki: &Pki, listed: bool) -> (Deployment, Deployment) {
    let (listener_a, address_a) = listener().await;
    let (listener_b, _) = listener().await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.tables = outbound_dialback(pki);
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.endpoints = vec![(HOME_A, address_a)];
    if listed {
        spec_b.dialback = vec![HOME_A];
        spec_b.tables = inbound_dialback();
    }
    let a = start(pki, &spec_a, listener_a).await;
    let b = start(pki, &spec_b, listener_b).await;
    (a, b)
}

/// A stream header from `from` to `to` that declares the dialback
/// namespace.
fn initial_header(from: &str, to: &str) -> StreamHeader {
    StreamHeader {
        from: HeaderAddress::Domain(domain(from)),
        to: HeaderAddress::Domain(domain(to)),
        id: None,
        version: Some(StreamVersion::V1_0),
        lang: Some(StreamLanguage::english()),
        declare_dialback: true,
    }
}

/// A response header from `from` to `to` that declares the dialback
/// namespace.
fn response_header(from: &str, to: &str) -> StreamHeader {
    let mut header =
        StreamHeader::response(domain(from), Some(domain(to)), StreamLanguage::english());
    header.declare_dialback = true;
    header
}

fn dialback_element(name: &str, from: &str, to: &str) -> minidom::ElementBuilder {
    Element::builder(name, JABBER_SERVER_DIALBACK)
        .attr(xml_ncname!("from").to_owned(), from)
        .attr(xml_ncname!("to").to_owned(), to)
}

async fn write_element<S: AsyncWrite + Unpin>(
    socket: &mut S,
    encoder: &mut StreamEncoder,
    element: &Element,
) {
    let bytes = encoder.element(element).expect("an encodable element");
    socket.write_all(&bytes).await.expect("write an element");
    socket.flush().await.expect("flush");
}

async fn open_stream<S: AsyncWrite + Unpin>(
    socket: &mut S,
    header: &StreamHeader,
) -> StreamEncoder {
    let (encoder, bytes) = StreamEncoder::open(header).expect("an encodable header");
    socket.write_all(&bytes).await.expect("write a header");
    socket.flush().await.expect("flush");
    encoder
}

/// Read until the other side closes, fails or `wait` passes.
async fn read_rest<S: AsyncRead + Unpin>(socket: &mut S, wait: Duration) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, socket.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) => return (bytes, true),
            Ok(Ok(read)) => bytes.extend_from_slice(&chunk[..read]),
            Err(_) => return (bytes, false),
        }
    }
}

/// The first-level elements of `bytes`, a stream that opens with a header.
fn elements_of(bytes: &[u8]) -> Vec<Element> {
    let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
    let _ = decoder.push(bytes);
    let mut elements = Vec::new();
    while let Ok(Some(item)) = decoder.next_item() {
        if let Inbound::Element(element) = item {
            elements.push(element);
        }
    }
    elements
}

/// Open a stream from `from` to the deployment at `address` and negotiate
/// STARTTLS. The plaintext part must succeed.
async fn starttls(address: SocketAddr, from: &str, to: &str) -> TcpStream {
    let mut socket = TcpStream::connect(address).await.expect("connect");
    let mut encoder = open_stream(&mut socket, &initial_header(from, to)).await;
    let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
    await_element(&mut socket, &mut decoder, "features", STREAM).await;
    write_element(
        &mut socket,
        &mut encoder,
        &Element::builder("starttls", TLS).build(),
    )
    .await;
    await_element(&mut socket, &mut decoder, "proceed", TLS).await;
    socket
}

/// A peer stream after TLS and the post-TLS header.
struct Negotiated<S> {
    tls: S,
    encoder: StreamEncoder,
    decoder: StreamDecoder,
    stream_id: StreamId,
    features: Element,
}

/// Connect to `address` as `from`, without a client certificate, through
/// STARTTLS, and read the post-TLS header and features.
async fn without_certificate(
    pki: &Pki,
    address: SocketAddr,
    from: &str,
    to: &str,
) -> Negotiated<tokio_rustls::client::TlsStream<TcpStream>> {
    let socket = starttls(address, from, to).await;
    let connector = TlsConnector::from(Arc::new(pki.client_config_without_certificate()));
    let server_name = ServerName::try_from(to.to_owned()).expect("a server name");
    let mut tls = connector
        .connect(server_name, socket)
        .await
        .expect("a TLS handshake without a client certificate");
    let encoder = open_stream(&mut tls, &initial_header(from, to)).await;
    let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
    let response = await_header(&mut tls, &mut decoder).await;
    let stream_id = response.id.expect("a stream ID");
    let features = await_element(&mut tls, &mut decoder, "features", STREAM).await;
    Negotiated {
        tls,
        encoder,
        decoder,
        stream_id,
        features,
    }
}

fn offers_dialback(features: &Element) -> bool {
    features
        .children()
        .any(|child| child.is("dialback", DIALBACK_FEATURES))
}

fn offers_sasl(features: &Element) -> bool {
    features
        .children()
        .any(|child| child.is("mechanisms", SASL))
}

fn dialback_stream(deployment: &Deployment, remote: &str) -> Option<VerifiedPeerStream> {
    deployment.handle.streams().into_iter().find(|stream| {
        stream.remote() == &domain(remote) && stream.strength() == AuthenticationStrength::Dialback
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_without_a_certificate_authenticates_by_dialback() {
    let pki = Pki::new();
    let (_a, b) = deployments(&pki, true).await;
    let mut peer = without_certificate(&pki, b.address, HOME_A, HOME_B).await;
    assert!(offers_dialback(&peer.features));
    assert!(!offers_sasl(&peer.features));

    // home-a's key for this stream: home-b verifies it with home-a.
    let key = secret_key(HOME_B, HOME_A, &peer.stream_id);
    let result = dialback_element("result", HOME_A, HOME_B)
        .append(key)
        .build();
    write_element(&mut peer.tls, &mut peer.encoder, &result).await;
    let answer = await_element(
        &mut peer.tls,
        &mut peer.decoder,
        "result",
        JABBER_SERVER_DIALBACK,
    )
    .await;
    assert_eq!(answer.attr("type"), Some("valid"));
    assert_eq!(answer.attr("from"), Some(HOME_B));
    assert_eq!(answer.attr("to"), Some(HOME_A));

    assert!(eventually(WAIT, || dialback_stream(&b, HOME_A).is_some()).await);
    let stream = dialback_stream(&b, HOME_A).expect("the dialback stream");
    assert_eq!(stream.direction(), StreamDirection::Inbound);
    assert_eq!(stream.local(), &domain(HOME_B));
    // No certificate was presented, so there is no certificate evidence.
    assert_eq!(stream.trust_profile(), None);
    assert_eq!(stream.certificate_sha256(), None);
    assert_eq!(stream.expires_at(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_the_authoritative_server_did_not_issue_is_refused() {
    let pki = Pki::new();
    let (_a, b) = deployments(&pki, true).await;
    let mut peer = without_certificate(&pki, b.address, HOME_A, HOME_B).await;
    // A key under another secret, as an impostor without home-a's secret
    // would send.
    let forged = DialbackSecret::new(b"an impostor's secret, not home-a's at all")
        .key(&domain(HOME_B), &domain(HOME_A), &peer.stream_id)
        .expect("a key");
    let result = dialback_element("result", HOME_A, HOME_B)
        .append(forged.as_str())
        .build();
    write_element(&mut peer.tls, &mut peer.encoder, &result).await;
    let answer = await_element(
        &mut peer.tls,
        &mut peer.decoder,
        "result",
        JABBER_SERVER_DIALBACK,
    )
    .await;
    assert_eq!(answer.attr("type"), Some("invalid"));
    let (_, closed) = read_rest(&mut peer.tls, WAIT).await;
    assert!(closed, "home-b closes the stream after an invalid key");
    assert!(dialback_stream(&b, HOME_A).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unlisted_domain_never_reaches_dialback() {
    let pki = Pki::new();
    let (_a, b) = deployments(&pki, false).await;
    let socket = starttls(b.address, HOME_A, HOME_B).await;
    let connector = TlsConnector::from(Arc::new(pki.client_config_without_certificate()));
    let server_name = ServerName::try_from(HOME_B.to_owned()).expect("a server name");
    // Without any dialback role the receiving side requires a client
    // certificate, so the handshake fails, at once or on the first read.
    if let Ok(mut tls) = connector.connect(server_name, socket).await {
        let header = initial_header(HOME_A, HOME_B);
        let (_, bytes) = StreamEncoder::open(&header).expect("a header");
        let _ = tls.write_all(&bytes).await;
        let (bytes, closed) = read_rest(&mut tls, WAIT).await;
        assert!(closed);
        assert!(
            !elements_of(&bytes)
                .iter()
                .any(|element| element.is("features", STREAM)),
            "no features after a refused handshake"
        );
    }
    assert!(b.handle.streams().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_certificate_is_never_downgraded_to_dialback() {
    let pki = Pki::new();
    let (_a, b) = deployments(&pki, true).await;
    let socket = starttls(b.address, HOME_A, HOME_B).await;
    // A certificate for another name, presented for home-a.test: an active
    // certificate failure.
    let connector = TlsConnector::from(Arc::new(pki.client_config("impostor.test")));
    let server_name = ServerName::try_from(HOME_B.to_owned()).expect("a server name");
    if let Ok(mut tls) = connector.connect(server_name, socket).await {
        let header = initial_header(HOME_A, HOME_B);
        let (_, bytes) = StreamEncoder::open(&header).expect("a header");
        let _ = tls.write_all(&bytes).await;
        let (bytes, closed) = read_rest(&mut tls, WAIT).await;
        assert!(closed);
        assert!(
            !elements_of(&bytes)
                .iter()
                .any(|element| element.is("features", STREAM)),
            "nothing is offered after an active certificate failure"
        );
    }
    assert!(b.handle.streams().is_empty());
}

/// What the hand-written receiving server for home-b.test learned.
struct ReceivingServerReport {
    /// The type of home-a's answer on the verification connection.
    verified: Option<String>,
    /// Whether home-a tried SASL EXTERNAL first.
    tried_external: bool,
}

/// What the hand-written receiving server offers after TLS.
#[derive(Clone, Copy)]
enum PostTlsOffer {
    /// Dialback only.
    Dialback,
    /// SASL EXTERNAL beside dialback, refused with `not-authorized` as a
    /// server that cannot verify the initiator's certificate refuses it
    /// (XEP-0178 §3, step 11b); dialback follows on the same stream.
    RefusedExternal,
}

/// A receiving server for home-b.test on `listener` that offers
/// `post_tls` after TLS, takes home-a's dialback key, verifies it with
/// home-a at `authoritative` on a connection without a client
/// certificate, and answers home-a with the result. It keeps the stream
/// open until `release` fires.
fn receiving_server(
    pki: &Pki,
    listener: TcpListener,
    authoritative: SocketAddr,
    post_tls: PostTlsOffer,
    release: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<ReceivingServerReport> {
    let acceptor = TlsAcceptor::from(Arc::new(pki.server_config(HOME_B, &[])));
    let verifier = TlsConnector::from(Arc::new(pki.client_config_without_certificate()));
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("home-a connects");
        let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
        await_header(&mut socket, &mut decoder).await;
        let mut encoder = open_stream(&mut socket, &response_header(HOME_B, HOME_A)).await;
        let offer = Element::builder("starttls", TLS)
            .append(Element::builder("required", TLS).build())
            .build();
        let features = Element::builder("features", STREAM).append(offer).build();
        write_element(&mut socket, &mut encoder, &features).await;
        await_element(&mut socket, &mut decoder, "starttls", TLS).await;
        write_element(
            &mut socket,
            &mut encoder,
            &Element::builder("proceed", TLS).build(),
        )
        .await;
        let mut tls = acceptor.accept(socket).await.expect("the TLS handshake");

        let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
        await_header(&mut tls, &mut decoder).await;
        let response = response_header(HOME_B, HOME_A);
        let stream_id = response.id.clone().expect("a stream ID");
        let mut encoder = open_stream(&mut tls, &response).await;
        let dialback = Element::builder("dialback", DIALBACK_FEATURES)
            .append(Element::builder("errors", DIALBACK_FEATURES).build())
            .build();
        let mut features = Element::builder("features", STREAM);
        if let PostTlsOffer::RefusedExternal = post_tls {
            let mechanisms = Element::builder("mechanisms", SASL)
                .append(
                    Element::builder("mechanism", SASL)
                        .append("EXTERNAL")
                        .build(),
                )
                .build();
            features = features.append(mechanisms);
        }
        let features = features.append(dialback).build();
        write_element(&mut tls, &mut encoder, &features).await;
        let mut tried_external = false;
        if let PostTlsOffer::RefusedExternal = post_tls {
            let auth = await_element(&mut tls, &mut decoder, "auth", SASL).await;
            assert_eq!(auth.attr("mechanism"), Some("EXTERNAL"));
            tried_external = true;
            let failure = Element::builder("failure", SASL)
                .append(Element::builder("not-authorized", SASL).build())
                .build();
            write_element(&mut tls, &mut encoder, &failure).await;
        }
        let result = await_element(&mut tls, &mut decoder, "result", JABBER_SERVER_DIALBACK).await;
        assert_eq!(result.attr("from"), Some(HOME_A));
        assert_eq!(result.attr("to"), Some(HOME_B));
        let key = result.text();

        // XEP-0220 §2.1.2: ask home-a's Authoritative Server.
        let socket = starttls(authoritative, HOME_B, HOME_A).await;
        let server_name = ServerName::try_from(HOME_A.to_owned()).expect("a server name");
        let mut verification = verifier
            .connect(server_name, socket)
            .await
            .expect("a TLS handshake with home-a");
        let mut verify_encoder =
            open_stream(&mut verification, &initial_header(HOME_B, HOME_A)).await;
        let mut verify_decoder = StreamDecoder::new(CodecLimits::PILOT);
        await_header(&mut verification, &mut verify_decoder).await;
        await_element(&mut verification, &mut verify_decoder, "features", STREAM).await;
        let request = dialback_element("verify", HOME_B, HOME_A)
            .attr(xml_ncname!("id").to_owned(), stream_id.as_str())
            .append(key.trim())
            .build();
        write_element(&mut verification, &mut verify_encoder, &request).await;
        let answer = await_element(
            &mut verification,
            &mut verify_decoder,
            "verify",
            JABBER_SERVER_DIALBACK,
        )
        .await;
        assert_eq!(answer.attr("from"), Some(HOME_A));
        assert_eq!(answer.attr("to"), Some(HOME_B));
        assert_eq!(answer.attr("id"), Some(stream_id.as_str()));
        let verified = answer.attr("type").map(str::to_owned);

        // Tell home-a what its Authoritative Server said.
        let outcome = verified.clone().unwrap_or_else(|| "invalid".to_owned());
        let reply = dialback_element("result", HOME_B, HOME_A)
            .attr(xml_ncname!("type").to_owned(), outcome)
            .build();
        write_element(&mut tls, &mut encoder, &reply).await;
        let _ = release.await;
        drop(tls);
        ReceivingServerReport {
            verified,
            tried_external,
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deployment_authenticates_itself_by_dialback_where_external_is_not_offered() {
    let pki = Pki::new();
    let (peer_listener, peer_address) = listener().await;
    let (listener_a, address_a) = listener().await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, peer_address)];
    spec_a.tables = outbound_dialback(&pki);
    let a = start(&pki, &spec_a, listener_a).await;
    assert_eq!(a.address, address_a);

    let (release, released) = tokio::sync::oneshot::channel();
    let server = receiving_server(
        &pki,
        peer_listener,
        address_a,
        PostTlsOffer::Dialback,
        released,
    );
    let stream = tokio::time::timeout(WAIT, a.handle.connect(&domain(HOME_A), &domain(HOME_B)))
        .await
        .expect("the attempt ends in time")
        .expect("a stream authenticated by dialback");
    assert_eq!(stream.strength(), AuthenticationStrength::Dialback);
    assert_eq!(stream.direction(), StreamDirection::Outbound);
    // home-a verified home-b's certificate whatever it authenticated by.
    assert_eq!(stream.trust_profile(), Some(TrustProfileKind::PrivateCa));

    let _ = release.send(());
    let report = server.await.expect("the receiving server");
    assert_eq!(report.verified.as_deref(), Some("valid"));
    assert!(!report.tried_external);
}

/// A receiving server that cannot verify home-a's certificate still offers
/// SASL EXTERNAL, refuses it, and expects dialback on the same stream, as
/// ejabberd does: home-a sends its key after the failure and authenticates
/// itself by dialback.
#[tokio::test(flavor = "multi_thread")]
async fn a_deployment_authenticates_itself_by_dialback_after_external_is_refused() {
    let pki = Pki::new();
    let (peer_listener, peer_address) = listener().await;
    let (listener_a, address_a) = listener().await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, peer_address)];
    spec_a.tables = outbound_dialback(&pki);
    let a = start(&pki, &spec_a, listener_a).await;
    assert_eq!(a.address, address_a);

    let (release, released) = tokio::sync::oneshot::channel();
    let server = receiving_server(
        &pki,
        peer_listener,
        address_a,
        PostTlsOffer::RefusedExternal,
        released,
    );
    let stream = tokio::time::timeout(WAIT, a.handle.connect(&domain(HOME_A), &domain(HOME_B)))
        .await
        .expect("the attempt ends in time")
        .expect("a stream authenticated by dialback after the refusal");
    assert_eq!(stream.strength(), AuthenticationStrength::Dialback);
    assert_eq!(stream.direction(), StreamDirection::Outbound);
    assert_eq!(stream.trust_profile(), Some(TrustProfileKind::PrivateCa));

    let _ = release.send(());
    let report = server.await.expect("the receiving server");
    assert!(report.tried_external, "EXTERNAL is tried first");
    assert_eq!(report.verified.as_deref(), Some("valid"));
}

/// Send home-a's genuine key for `peer`'s stream to home-b.test and return
/// home-b's typed answer.
async fn send_key(peer: &mut Negotiated<tokio_rustls::client::TlsStream<TcpStream>>) -> Element {
    let key = secret_key(HOME_B, HOME_A, &peer.stream_id);
    let result = dialback_element("result", HOME_A, HOME_B)
        .append(key)
        .build();
    write_element(&mut peer.tls, &mut peer.encoder, &result).await;
    await_element(
        &mut peer.tls,
        &mut peer.decoder,
        "result",
        JABBER_SERVER_DIALBACK,
    )
    .await
}

/// Whether a `type='error'` dialback answer carries `condition`.
fn has_condition(answer: &Element, condition: &str) -> bool {
    answer.attr("type") == Some("error")
        && answer
            .children()
            .filter(|child| child.name() == "error")
            .flat_map(|error| error.children())
            .any(|child| child.is(condition, STANZAS))
}

/// `[[endpoints]]` lines for a direct-TLS endpoint.
fn direct_tls_endpoint(domain: &str, address: SocketAddr) -> Vec<String> {
    vec![
        "[[endpoints]]".to_owned(),
        format!("domain = {domain:?}"),
        format!("address = \"{address}\""),
        "transport = \"direct_tls\"".to_owned(),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn an_authoritative_server_whose_certificate_fails_is_not_acceptable() {
    let pki = Pki::new();
    // Where home-a.test's Authoritative Server should be, a direct-TLS
    // server presents a certificate for another name: an active
    // certificate failure on the verification connection.
    let (impostor, impostor_address) = listener().await;
    let acceptor = TlsAcceptor::from(Arc::new(pki.server_config("impostor.test", &[])));
    let reached = tokio::spawn(async move {
        let Ok((socket, _)) = impostor.accept().await else {
            return false;
        };
        let _ = acceptor.accept(socket).await;
        true
    });
    let (listener_b, _) = listener().await;
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.dialback = vec![HOME_A];
    spec_b.tables = inbound_dialback();
    spec_b
        .tables
        .extend(direct_tls_endpoint(HOME_A, impostor_address));
    let b = start(&pki, &spec_b, listener_b).await;

    let mut peer = without_certificate(&pki, b.address, HOME_A, HOME_B).await;
    let answer = send_key(&mut peer).await;
    assert!(has_condition(&answer, "not-acceptable"), "{answer:?}");
    assert!(
        tokio::time::timeout(WAIT, reached)
            .await
            .expect("the verification connection in time")
            .expect("the impostor task"),
        "the verification connection reached the impostor"
    );
    let (_, closed) = read_rest(&mut peer.tls, WAIT).await;
    assert!(closed, "home-b closes the stream after a dialback error");
    assert!(dialback_stream(&b, HOME_A).is_none());
}

/// A resolver that answers every domain with one SRV-derived endpoint.
struct SrvResolver(SocketAddr);

#[async_trait]
impl EndpointResolver for SrvResolver {
    async fn resolve(&self, _domain: &CanonicalDomain) -> Result<Endpoints, ResolveError> {
        Ok(Endpoints::new(vec![Endpoint {
            address: self.0,
            origin: EndpointOrigin::Srv,
            transport: OutboundTransport::StartTls,
        }]))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verification_connection_never_dials_a_blocked_address() {
    let pki = Pki::new();
    // home-a.test's real Authoritative Server, on loopback: where a DNS
    // answer would point, and a blocked class for a DNS-derived address.
    let (listener_a, address_a) = listener().await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.tables = outbound_dialback(&pki);
    let a = start(&pki, &spec_a, listener_a).await;
    let (listener_b, _) = listener().await;
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.dialback = vec![HOME_A];
    spec_b.tables = inbound_dialback();
    let options = ServiceOptions {
        resolver: Some(Arc::new(SrvResolver(address_a)) as Arc<dyn EndpointResolver>),
        ..Default::default()
    };
    let b = start_with(&pki, &spec_b, listener_b, options).await;

    let mut peer = without_certificate(&pki, b.address, HOME_A, HOME_B).await;
    let answer = send_key(&mut peer).await;
    // Every address is refused, so there is no Authoritative Server this
    // deployment may reach.
    assert!(
        has_condition(&answer, "remote-server-not-found"),
        "{answer:?}"
    );
    assert!(dialback_stream(&b, HOME_A).is_none());
    // home-a never saw a verification connection.
    assert!(a.handle.streams().is_empty());
    assert_eq!(a.handle.open_sockets(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn verifications_beyond_their_rate_are_a_resource_constraint() {
    let pki = Pki::new();
    let (a, b) = {
        let (listener_a, address_a) = listener().await;
        let (listener_b, _) = listener().await;
        let mut spec_a = Spec::new(HOME_A, HOME_B);
        spec_a.tables = outbound_dialback(&pki);
        let mut spec_b = Spec::new(HOME_B, HOME_A);
        spec_b.endpoints = vec![(HOME_A, address_a)];
        spec_b.dialback = vec![HOME_A];
        spec_b.tables = inbound_dialback();
        spec_b.limits = vec![
            "verification_per_second = 1".to_owned(),
            "verification_burst = 1".to_owned(),
        ];
        (
            start(&pki, &spec_a, listener_a).await,
            start(&pki, &spec_b, listener_b).await,
        )
    };
    // Both peers are connected before the first key is sent, so only the
    // first verification runs between the two keys, well within the
    // second it takes the rate to accrue a token. The first key is forged:
    // its verification spends the only token and is refused, so no stream
    // holds the pair.
    let mut first = without_certificate(&pki, b.address, HOME_A, HOME_B).await;
    let mut second = without_certificate(&pki, b.address, HOME_A, HOME_B).await;
    let forged = DialbackSecret::new(b"an impostor's secret, not home-a's at all")
        .key(&domain(HOME_B), &domain(HOME_A), &first.stream_id)
        .expect("a key");
    let result = dialback_element("result", HOME_A, HOME_B)
        .append(forged.as_str())
        .build();
    write_element(&mut first.tls, &mut first.encoder, &result).await;
    let answer = await_element(
        &mut first.tls,
        &mut first.decoder,
        "result",
        JABBER_SERVER_DIALBACK,
    )
    .await;
    assert_eq!(answer.attr("type"), Some("invalid"));

    // A genuine key at once: the verification rate has no token left.
    let answer = send_key(&mut second).await;
    assert!(has_condition(&answer, "resource-constraint"), "{answer:?}");
    assert!(dialback_stream(&b, HOME_A).is_none());
    // Ordinary dials were never charged: home-b still reaches home-a.
    b.handle
        .connect(&domain(HOME_B), &domain(HOME_A))
        .await
        .expect("an ordinary outbound stream");
    drop(a);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dialback_stream_never_replaces_a_pkix_stream() {
    let pki = Pki::new();
    let (listener_a, _) = listener().await;
    let (listener_b, address_b) = listener().await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, address_b)];
    spec_a.tables = outbound_dialback(&pki);
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.dialback = vec![HOME_A];
    spec_b.tables = inbound_dialback();
    // Any stream could replace another at once, but for its strength.
    spec_b.limits = vec!["replacement_interval_ms = 1".to_owned()];
    let a = start(&pki, &spec_a, listener_a).await;
    let b = start(&pki, &spec_b, listener_b).await;

    // home-a.test authenticates to home-b.test with its certificate.
    a.handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("a pkix stream");
    let pkix = |deployment: &Deployment| {
        deployment.handle.streams().into_iter().find(|stream| {
            stream.direction() == StreamDirection::Inbound
                && stream.remote() == &domain(HOME_A)
                && stream.strength() == AuthenticationStrength::Pkix
        })
    };
    assert!(eventually(WAIT, || pkix(&b).is_some()).await);
    let kept = pkix(&b).expect("the pkix stream").connection();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A dialback attempt for the same pair is refused before any key is
    // verified, with `conflict`, and the pkix stream stays.
    let mut peer = without_certificate(&pki, b.address, HOME_A, HOME_B).await;
    let key = secret_key(HOME_B, HOME_A, &peer.stream_id);
    let result = dialback_element("result", HOME_A, HOME_B)
        .append(key)
        .build();
    write_element(&mut peer.tls, &mut peer.encoder, &result).await;
    let (bytes, closed) = read_rest(&mut peer.tls, WAIT).await;
    assert!(closed);
    peer.decoder.push(&bytes).expect("input within the limits");
    let mut conflict = false;
    while let Ok(Some(item)) = peer.decoder.next_item() {
        if let Inbound::Element(element) = item {
            if element.is("error", STREAM) {
                conflict = element.children().any(|child| child.name() == "conflict");
            }
        }
    }
    assert!(
        conflict,
        "home-b refuses the dialback stream with `conflict`"
    );
    assert_eq!(
        pkix(&b).map(|stream| stream.connection()),
        Some(kept),
        "the pkix stream is kept"
    );
    assert!(dialback_stream(&b, HOME_A).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn verifications_beyond_their_cap_are_a_resource_constraint() {
    let pki = Pki::new();
    // home-a.test's Authoritative Server accepts TCP and never answers, so
    // a verification stays in progress until its deadline.
    let (silent, silent_address) = listener().await;
    let held = tokio::spawn(async move {
        let mut sockets = Vec::new();
        while let Ok((socket, _)) = silent.accept().await {
            sockets.push(socket);
        }
    });
    let (listener_b, _) = listener().await;
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.endpoints = vec![(HOME_A, silent_address)];
    spec_b.dialback = vec![HOME_A];
    spec_b.tables = inbound_dialback();
    spec_b.limits = vec!["max_verifications = 1".to_owned()];
    let b = start(&pki, &spec_b, listener_b).await;

    let mut first = without_certificate(&pki, b.address, HOME_A, HOME_B).await;
    let mut second = without_certificate(&pki, b.address, HOME_A, HOME_B).await;
    let key = secret_key(HOME_B, HOME_A, &first.stream_id);
    let result = dialback_element("result", HOME_A, HOME_B)
        .append(key)
        .build();
    write_element(&mut first.tls, &mut first.encoder, &result).await;
    // The first verification holds the only slot.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let answer = send_key(&mut second).await;
    assert!(has_condition(&answer, "resource-constraint"), "{answer:?}");
    assert!(dialback_stream(&b, HOME_A).is_none());
    held.abort();
}
