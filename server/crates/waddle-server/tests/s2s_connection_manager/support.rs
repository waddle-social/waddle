//! Two in-process S2S deployments on loopback: a test CA, PEM files and
//! configuration files in a temporary directory, configuration snapshots
//! loaded through the production loader with a fixed clock, and services
//! started on real loopback listeners. The CA is every peer's private-CA
//! profile, so no public root is involved. A hand-written peer
//! ([`RawPeer`]) negotiates STARTTLS and SASL EXTERNAL with a deployment
//! and then writes whatever a test gives it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use minidom::Element;
use rcgen::{
    date_time_ymd, BasicConstraints, Certificate, CertificateParams,
    CertificateRevocationListParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyIdMethod, KeyPair, KeyUsagePurpose, SerialNumber,
};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::{ClientConfig, RootCertStore};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use waddle_server::s2s::config::{ConfigError, S2sConfig};
use waddle_server::s2s::conn::{
    S2sHandle, S2sService, ServiceOptions, SinkVerdict, StanzaSink, VerifiedPeerStream,
};
use waddle_server::s2s::verify::TlsSettings;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::ns::{SASL, STREAM, TLS};
use waddle_xmpp::s2s::{
    CodecLimits, HeaderAddress, Inbound, StreamDecoder, StreamEncoder, StreamErrorCondition,
    StreamHeader, StreamLanguage, StreamVersion,
};

pub const HOME_A: &str = "home-a.test";
pub const HOME_B: &str = "home-b.test";

/// The fixed time every certificate and CRL is checked against:
/// 2030-06-15T00:00:00Z.
const NOW_SECS: u64 = 1_907_712_000;

#[derive(Debug)]
struct FixedTime;

impl TimeProvider for FixedTime {
    fn current_time(&self) -> Option<UnixTime> {
        Some(UnixTime::since_unix_epoch(Duration::from_secs(NOW_SECS)))
    }
}

/// TLS settings with the ring provider and the fixed clock.
pub fn settings() -> TlsSettings {
    TlsSettings::new(Arc::new(default_provider())).with_time_provider(Arc::new(FixedTime))
}

/// The canonical form of a test domain.
pub fn domain(text: &str) -> CanonicalDomain {
    CanonicalDomain::parse_exact(text).expect("a canonical test domain")
}

fn name(common_name: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name
}

/// A test CA, its PEM files and the leaf credentials it issues.
pub struct Pki {
    pub dir: TempDir,
    certificate: Certificate,
    key: KeyPair,
    pub ca_path: PathBuf,
    pub crl_path: PathBuf,
}

impl Pki {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut params = CertificateParams::default();
        params.distinguished_name = name("S2S Test CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        params.not_before = date_time_ymd(2030, 1, 1);
        params.not_after = date_time_ymd(2031, 1, 1);
        params.use_authority_key_identifier_extension = true;
        let key = KeyPair::generate().expect("a CA key");
        let certificate = params.self_signed(&key).expect("a CA certificate");
        let crl = CertificateRevocationListParams {
            this_update: date_time_ymd(2030, 6, 12),
            next_update: date_time_ymd(2030, 6, 19),
            crl_number: SerialNumber::from(1u64),
            issuing_distribution_point: None,
            revoked_certs: Vec::new(),
            key_identifier_method: KeyIdMethod::Sha256,
        }
        .signed_by(&certificate, &key)
        .expect("a CRL");
        let ca_path = write(&dir, "ca.pem", &certificate.pem());
        let crl_path = write(&dir, "ca.crl.pem", &crl.pem().expect("PEM"));
        Self {
            dir,
            certificate,
            key,
            ca_path,
            crl_path,
        }
    }

    /// A leaf certificate for `names`, valid in both TLS roles, and its key.
    fn issue(&self, names: &[&str]) -> (Certificate, KeyPair) {
        let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        let mut params = CertificateParams::new(names).expect("DNS names");
        params.distinguished_name = name("s2s test server");
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        params.not_before = date_time_ymd(2030, 6, 1);
        params.not_after = date_time_ymd(2030, 7, 1);
        params.use_authority_key_identifier_extension = true;
        let key = KeyPair::generate().expect("a leaf key");
        let certificate = params
            .signed_by(&key, &self.certificate, &self.key)
            .expect("a leaf");
        (certificate, key)
    }

    /// A certificate for `names` and its key, written as PEM files.
    pub fn credentials(&self, file: &str, names: &[&str]) -> (PathBuf, PathBuf) {
        let (certificate, key) = self.issue(names);
        (
            write(&self.dir, &format!("{file}.chain.pem"), &certificate.pem()),
            write(&self.dir, &format!("{file}.key.pem"), &key.serialize_pem()),
        )
    }

    /// TLS client settings for a hand-written peer authenticating as
    /// `local`: a certificate for it from this CA, this CA as the only
    /// root, and the fixed clock.
    pub fn client_config(&self, local: &str) -> ClientConfig {
        let (certificate, key) = self.issue(&[local]);
        let mut roots = RootCertStore::empty();
        roots
            .add(self.certificate.der().clone())
            .expect("the test CA as a root");
        ClientConfig::builder_with_details(Arc::new(default_provider()), Arc::new(FixedTime))
            .with_safe_default_protocol_versions()
            .expect("the provider's protocol versions")
            .with_root_certificates(roots)
            .with_client_auth_cert(
                vec![certificate.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .expect("a client certificate")
    }

    /// Write a file into the PKI's directory.
    pub fn file(&self, file: &str, contents: &str) -> PathBuf {
        write(&self.dir, file, contents)
    }
}

fn write(dir: &TempDir, file: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(file);
    std::fs::write(&path, contents).expect("write a test file");
    path
}

/// What one deployment's configuration says, written as TOML by
/// [`Spec::toml`].
pub struct Spec {
    pub local: Vec<&'static str>,
    pub listen: Option<String>,
    pub mode: &'static str,
    pub allow: Vec<&'static str>,
    pub deny: Vec<&'static str>,
    pub dialback: Vec<&'static str>,
    pub trusted_homes: Vec<&'static str>,
    pub require_private_ca: Vec<&'static str>,
    /// Remote domains that use the test CA as their private-CA profile.
    pub private: Vec<&'static str>,
    pub endpoints: Vec<(&'static str, SocketAddr)>,
    /// Lines of the `[limits]` table.
    pub limits: Vec<String>,
}

impl Spec {
    /// A deployment serving `local`, admitting `peer` by allowlist and
    /// trusting the test CA for it.
    pub fn new(local: &'static str, peer: &'static str) -> Self {
        Self {
            local: vec![local],
            listen: Some("127.0.0.1:0".to_owned()),
            mode: "allowlist",
            allow: vec![peer],
            deny: Vec::new(),
            dialback: Vec::new(),
            trusted_homes: Vec::new(),
            require_private_ca: Vec::new(),
            private: vec![peer],
            endpoints: Vec::new(),
            limits: Vec::new(),
        }
    }

    pub fn toml(&self, pki: &Pki) -> String {
        let list = |entries: &[&str]| {
            let quoted: Vec<String> = entries.iter().map(|entry| format!("{entry:?}")).collect();
            format!("[{}]", quoted.join(", "))
        };
        let mut lines = vec![
            "generation = \"0b8f2d4e-6a59-4f1c-9a43-3f1d2c7e5a10\"".to_owned(),
            format!("local_domains = {}", list(&self.local)),
        ];
        if let Some(listen) = &self.listen {
            lines.push(format!("listen = {listen:?}"));
        }
        for local in &self.local {
            let (chain, key) = pki.credentials(local, &[local]);
            lines.push("[[credentials]]".to_owned());
            lines.push(format!("domain = {local:?}"));
            lines.push(format!(
                "certificate_chain = {:?}",
                chain.display().to_string()
            ));
            lines.push(format!("private_key = {:?}", key.display().to_string()));
        }
        if !self.private.is_empty() {
            lines.push("[[private_ca]]".to_owned());
            lines.push(format!("domains = {}", list(&self.private)));
            lines.push(format!(
                "anchors = [{:?}]",
                pki.ca_path.display().to_string()
            ));
            lines.push(format!("crls = [{:?}]", pki.crl_path.display().to_string()));
        }
        for (domain, address) in &self.endpoints {
            lines.push("[[endpoints]]".to_owned());
            lines.push(format!("domain = {domain:?}"));
            lines.push(format!("address = \"{address}\""));
        }
        lines.push("[admission]".to_owned());
        lines.push(format!("mode = {:?}", self.mode));
        lines.push(format!("allow = {}", list(&self.allow)));
        lines.push(format!("deny = {}", list(&self.deny)));
        lines.push(format!("dialback = {}", list(&self.dialback)));
        lines.push(format!("trusted_homes = {}", list(&self.trusted_homes)));
        lines.push(format!(
            "require_private_ca = {}",
            list(&self.require_private_ca)
        ));
        lines.push("[limits]".to_owned());
        lines.extend(self.limits.iter().cloned());
        lines.join("\n")
    }

    /// Write the configuration and load it with the fixed clock.
    pub async fn load(&self, pki: &Pki) -> Result<S2sConfig, ConfigError> {
        load_text(pki, &self.toml(pki)).await
    }
}

/// Write `text` as a configuration file and load it.
pub async fn load_text(pki: &Pki, text: &str) -> Result<S2sConfig, ConfigError> {
    let path = pki.file("s2s.toml", text);
    S2sConfig::load(&path, settings()).await
}

/// A running deployment.
pub struct Deployment {
    pub service: S2sService,
    pub handle: S2sHandle,
    pub address: SocketAddr,
}

/// A loopback listener for a deployment, bound before its peer's
/// configuration names it as an endpoint.
pub async fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback listener");
    let address = listener.local_addr().expect("a bound address");
    (listener, address)
}

/// A loopback port that is bound but not listening, so a connection to it
/// is refused, held for as long as the returned socket lives: no other
/// test can start listening on it meanwhile.
pub fn dead_port() -> (TcpSocket, SocketAddr) {
    let socket = TcpSocket::new_v4().expect("a socket");
    socket
        .bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("bind a loopback port");
    let address = socket.local_addr().expect("a bound address");
    (socket, address)
}

/// Start a deployment from `spec` on `listener`.
pub async fn start(pki: &Pki, spec: &Spec, listener: TcpListener) -> Deployment {
    start_with(pki, spec, listener, ServiceOptions::default()).await
}

/// Start a deployment from `spec` on `listener` with `options`.
pub async fn start_with(
    pki: &Pki,
    spec: &Spec,
    listener: TcpListener,
    options: ServiceOptions,
) -> Deployment {
    let config = spec.load(pki).await.expect("a valid configuration");
    let service = S2sService::start(Arc::new(config), Some(listener), options);
    let address = service.local_address().expect("a listening service");
    let handle = service.handle();
    Deployment {
        service,
        handle,
        address,
    }
}

/// `home-a.test` and `home-b.test`, each admitting and dialling the other.
/// `adjust` changes either specification before it is loaded.
pub async fn pair(pki: &Pki, adjust: impl Fn(&mut Spec)) -> (Deployment, Deployment) {
    let (listener_a, address_a) = listener().await;
    let (listener_b, address_b) = listener().await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, address_b)];
    adjust(&mut spec_a);
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.endpoints = vec![(HOME_A, address_a)];
    adjust(&mut spec_b);
    let a = start(pki, &spec_a, listener_a).await;
    let b = start(pki, &spec_b, listener_b).await;
    (a, b)
}

/// An initiating stream header from `from` to `to`.
fn stream_header(from: Option<&str>, to: &str) -> StreamHeader {
    StreamHeader {
        from: HeaderAddress::from(from.map(domain)),
        to: HeaderAddress::Domain(domain(to)),
        id: None,
        version: Some(StreamVersion::V1_0),
        lang: Some(StreamLanguage::english()),
        declare_dialback: false,
    }
}

/// A plaintext initial stream header, built with the codec's encoder.
pub fn header(from: Option<&str>, to: &str) -> Vec<u8> {
    StreamEncoder::open(&stream_header(from, to))
        .expect("an encodable stream header")
        .1
}

/// What a raw peer received, and whether the server closed the connection
/// before the reader stopped waiting.
#[derive(Debug)]
pub struct Received {
    pub bytes: Vec<u8>,
    pub closed: bool,
    /// When the reader stopped: at the close or when the wait ended.
    pub at: Instant,
}

impl Received {
    /// The first-level elements the server sent, decoded as a peer would.
    pub fn elements(&self) -> Vec<Element> {
        let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
        if decoder.push(&self.bytes).is_err() {
            return Vec::new();
        }
        let mut elements = Vec::new();
        while let Ok(Some(item)) = decoder.next_item() {
            if let Inbound::Element(element) = item {
                elements.push(element);
            }
        }
        elements
    }

    /// The condition of the stream error the server sent, if it sent one.
    pub fn stream_error(&self) -> Option<StreamErrorCondition> {
        self.elements()
            .into_iter()
            .find(|element| element.is("error", STREAM))
            .map(StreamErrorCondition::from_peer_element)
    }

    /// Whether the server offered STARTTLS.
    pub fn offers_starttls(&self) -> bool {
        self.elements().iter().any(|element| {
            element.is("features", STREAM)
                && element.children().any(|child| child.is("starttls", TLS))
        })
    }

    /// Whether the server closed the connection without sending anything.
    pub fn closed_without_xml(&self) -> bool {
        self.closed && self.bytes.is_empty()
    }
}

/// Send `bytes` to `address` as a raw peer and collect what comes back
/// until the server closes the connection or `wait` passes.
pub async fn raw_exchange(address: SocketAddr, bytes: &[u8], wait: Duration) -> Received {
    let mut socket = TcpStream::connect(address).await.expect("connect");
    socket.write_all(bytes).await.expect("write");
    read_until_closed(&mut socket, wait).await
}

/// Send `bytes` to `address` as a raw peer and collect what comes back
/// until `done` holds, the server closes the connection or `wait` passes.
pub async fn raw_exchange_until(
    address: SocketAddr,
    bytes: &[u8],
    wait: Duration,
    done: impl Fn(&Received) -> bool,
) -> Received {
    let mut socket = TcpStream::connect(address).await.expect("connect");
    socket.write_all(bytes).await.expect("write");
    let deadline = tokio::time::Instant::now() + wait;
    let mut received = Received {
        bytes: Vec::new(),
        closed: false,
        at: Instant::now(),
    };
    let mut chunk = [0u8; 4096];
    loop {
        match tokio::time::timeout_at(deadline, socket.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) => {
                received.closed = true;
                break;
            }
            Ok(Ok(read)) => {
                received.bytes.extend_from_slice(&chunk[..read]);
                if done(&received) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    received.at = Instant::now();
    received
}

/// Read until the peer closes the connection or `wait` passes.
pub async fn read_until_closed(socket: &mut TcpStream, wait: Duration) -> Received {
    let mut bytes = Vec::new();
    let closed = tokio::time::timeout(wait, socket.read_to_end(&mut bytes))
        .await
        .is_ok();
    Received {
        bytes,
        closed,
        at: Instant::now(),
    }
}

/// Wait until `check` holds, for at most `wait`.
pub async fn eventually(wait: Duration, check: impl Fn() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + wait;
    while tokio::time::Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    check()
}

/// Read from `socket` into `decoder` until it yields the first-level
/// element `name` in `namespace`, skipping stream headers and other
/// elements before it.
async fn expect_element<S: AsyncRead + Unpin>(
    socket: &mut S,
    decoder: &mut StreamDecoder,
    name: &str,
    namespace: &str,
) -> Element {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut chunk = [0u8; 4096];
    loop {
        while let Some(item) = decoder.next_item().expect("well-formed input") {
            if let Inbound::Element(element) = item {
                assert!(
                    !element.is("error", STREAM),
                    "a stream error while waiting for <{name}/>: {:?}",
                    StreamErrorCondition::from_peer_element(element.clone())
                );
                if element.is(name, namespace) {
                    return element;
                }
            }
        }
        let read = tokio::time::timeout_at(deadline, socket.read(&mut chunk))
            .await
            .expect("the deployment answers in time")
            .expect("read");
        assert!(read > 0, "the deployment closed while awaiting <{name}/>");
        decoder
            .push(&chunk[..read])
            .expect("input within the limits");
    }
}

/// A hand-written peer on one connection: it negotiates STARTTLS and SASL
/// EXTERNAL with a deployment as one local domain, with a certificate for
/// it from the test CA, and then writes what a test gives it on the
/// established stream.
pub struct RawPeer {
    tls: TlsStream<TcpStream>,
    encoder: StreamEncoder,
    decoder: StreamDecoder,
}

impl RawPeer {
    /// Establish a stream from `local` to `remote` at `address`. The
    /// deployment registers the stream when it reads the last header sent
    /// here, which can be just after this returns.
    pub async fn establish(pki: &Pki, address: SocketAddr, local: &str, remote: &str) -> Self {
        let header = stream_header(Some(local), remote);
        let mut socket = TcpStream::connect(address).await.expect("connect");
        let (mut encoder, opening) = StreamEncoder::open(&header).expect("a stream header");
        socket.write_all(&opening).await.expect("write a header");
        let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
        expect_element(&mut socket, &mut decoder, "features", STREAM).await;
        let starttls = Element::builder("starttls", TLS).build();
        let bytes = encoder.element(&starttls).expect("an encodable element");
        socket.write_all(&bytes).await.expect("write STARTTLS");
        expect_element(&mut socket, &mut decoder, "proceed", TLS).await;

        let connector = TlsConnector::from(Arc::new(pki.client_config(local)));
        let server_name = ServerName::try_from(remote.to_owned()).expect("a server name");
        let mut tls = connector
            .connect(server_name, socket)
            .await
            .expect("the TLS handshake");

        let (mut encoder, opening) = StreamEncoder::open(&header).expect("a stream header");
        tls.write_all(&opening).await.expect("write a header");
        let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
        expect_element(&mut tls, &mut decoder, "features", STREAM).await;
        // An initial response of `=` is an empty one (RFC 6120 §6.4.2):
        // no authorization identity.
        let auth = Element::builder("auth", SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "EXTERNAL",
            )
            .append("=")
            .build();
        let bytes = encoder.element(&auth).expect("an encodable element");
        tls.write_all(&bytes)
            .await
            .expect("write the SASL exchange");
        expect_element(&mut tls, &mut decoder, "success", SASL).await;

        let (encoder, opening) = StreamEncoder::open(&header).expect("a stream header");
        tls.write_all(&opening).await.expect("write a header");
        let mut decoder = StreamDecoder::new(CodecLimits::PILOT);
        expect_element(&mut tls, &mut decoder, "features", STREAM).await;
        Self {
            tls,
            encoder,
            decoder,
        }
    }

    /// `element` serialised as a first-level element of the stream.
    pub fn encode(&mut self, element: &Element) -> Vec<u8> {
        self.encoder.element(element).expect("an encodable element")
    }

    /// Write `bytes` on the established stream.
    pub async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.tls.write_all(bytes).await?;
        self.tls.flush().await
    }

    /// Read until the deployment closes the connection or `wait` passes:
    /// the stream error it sent, if any, whether it closed, and when the
    /// reader stopped.
    pub async fn read_until_closed(
        &mut self,
        wait: Duration,
    ) -> (Option<StreamErrorCondition>, bool, Instant) {
        let deadline = tokio::time::Instant::now() + wait;
        let mut error = None;
        let mut chunk = [0u8; 4096];
        let closed = loop {
            while let Ok(Some(item)) = self.decoder.next_item() {
                if let Inbound::Element(element) = item {
                    if element.is("error", STREAM) {
                        error = Some(StreamErrorCondition::from_peer_element(element));
                    }
                }
            }
            match tokio::time::timeout_at(deadline, self.tls.read(&mut chunk)).await {
                Ok(Ok(0)) | Ok(Err(_)) => break true,
                Ok(Ok(read)) => {
                    if self.decoder.push(&chunk[..read]).is_err() {
                        break true;
                    }
                }
                Err(_) => break false,
            }
        };
        while let Ok(Some(item)) = self.decoder.next_item() {
            if let Inbound::Element(element) = item {
                if element.is("error", STREAM) {
                    error = Some(StreamErrorCondition::from_peer_element(element));
                }
            }
        }
        (error, closed, Instant::now())
    }
}

/// A stanza sink that keeps every stanza it is offered and refuses it, as
/// the sink of this release does.
#[derive(Debug, Default)]
pub struct RecordingSink {
    stanzas: Mutex<Vec<Element>>,
}

impl RecordingSink {
    /// The stanzas offered so far.
    pub fn stanzas(&self) -> Vec<Element> {
        match self.stanzas.lock() {
            Ok(stanzas) => stanzas.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// How many stanzas were offered so far.
    pub fn count(&self) -> usize {
        match self.stanzas.lock() {
            Ok(stanzas) => stanzas.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }
}

impl StanzaSink for RecordingSink {
    fn offer(&self, _stream: &VerifiedPeerStream, stanza: Element) -> SinkVerdict {
        match self.stanzas.lock() {
            Ok(mut stanzas) => stanzas.push(stanza),
            Err(poisoned) => poisoned.into_inner().push(stanza),
        }
        SinkVerdict::Refuse
    }
}
