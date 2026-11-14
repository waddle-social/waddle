//! Two in-process S2S deployments on loopback: a test CA, PEM files and
//! configuration files in a temporary directory, configuration snapshots
//! loaded through the production loader with a fixed clock, and services
//! started on real loopback listeners. The CA is every peer's private-CA
//! profile, so no public root is involved.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use minidom::Element;
use rcgen::{
    date_time_ymd, BasicConstraints, Certificate, CertificateParams,
    CertificateRevocationListParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyIdMethod, KeyPair, KeyUsagePurpose, SerialNumber,
};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::UnixTime;
use rustls::time_provider::TimeProvider;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use waddle_server::s2s::config::{ConfigError, S2sConfig};
use waddle_server::s2s::conn::{S2sHandle, S2sService, ServiceOptions};
use waddle_server::s2s::verify::TlsSettings;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::ns::{STREAM, TLS};
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

    /// A certificate for `names` and its key, written as PEM files.
    pub fn credentials(&self, file: &str, names: &[&str]) -> (PathBuf, PathBuf) {
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
        (
            write(&self.dir, &format!("{file}.chain.pem"), &certificate.pem()),
            write(&self.dir, &format!("{file}.key.pem"), &key.serialize_pem()),
        )
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

/// Start a deployment from `spec` on `listener`.
pub async fn start(pki: &Pki, spec: &Spec, listener: TcpListener) -> Deployment {
    let config = spec.load(pki).await.expect("a valid configuration");
    let service = S2sService::start(Arc::new(config), Some(listener), ServiceOptions::default());
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

/// A plaintext initial stream header, built with the codec's encoder.
pub fn header(from: Option<&str>, to: &str) -> Vec<u8> {
    let header = StreamHeader {
        from: HeaderAddress::from(from.map(domain)),
        to: HeaderAddress::Domain(domain(to)),
        id: None,
        version: Some(StreamVersion::V1_0),
        lang: Some(StreamLanguage::english()),
        declare_dialback: false,
    };
    StreamEncoder::open(&header)
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
