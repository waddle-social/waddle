//! The `.test` certificate authority, generated at harness start.
//!
//! The servers run on the real clock, so every certificate and CRL is
//! dated relative to the moment the harness starts: the CA from an hour
//! ago for thirty days, leaves for seven days and one complete CRL whose
//! `nextUpdate` is six days after its `thisUpdate`, inside the seven-day
//! ceiling of Waddle's private-CA profile. A second, unrelated CA issues
//! the certificates no peer trusts. Leaves carry `digitalSignature` and
//! either both TLS usages or serverAuth alone (ADR-0020 D2 accepts the
//! latter in both roles).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, CertificateRevocationListParams,
    DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyIdMethod, KeyPair,
    KeyUsagePurpose, SerialNumber,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::ring::default_provider;
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::WebPkiClientVerifier;
use rustls::{
    ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme,
    SupportedProtocolVersion,
};
use time::{Duration, OffsetDateTime};

/// Which CA issues a leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Issuer {
    /// The `.test` CA every private-CA profile in the harness trusts.
    TestCa,
    /// A CA nobody trusts.
    Other,
}

/// The extended key usages of a leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    ServerAndClient,
    ServerOnly,
}

/// The validity window of a leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    Current,
    /// Expired a day before the harness started.
    Expired,
}

/// What to issue.
#[derive(Debug, Clone)]
pub struct LeafSpec {
    /// The file stem the PEM files are written under.
    pub stem: String,
    /// DNS-IDs.
    pub names: Vec<String>,
    pub usage: Usage,
    pub validity: Validity,
    pub issuer: Issuer,
}

impl LeafSpec {
    /// A current dual-usage leaf from the `.test` CA for `names`.
    pub fn new(stem: &str, names: &[&str]) -> Self {
        Self {
            stem: stem.to_owned(),
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            usage: Usage::ServerAndClient,
            validity: Validity::Current,
            issuer: Issuer::TestCa,
        }
    }

    pub fn server_only(mut self) -> Self {
        self.usage = Usage::ServerOnly;
        self
    }

    pub fn expired(mut self) -> Self {
        self.validity = Validity::Expired;
        self
    }

    pub fn issued_by(mut self, issuer: Issuer) -> Self {
        self.issuer = issuer;
        self
    }
}

/// An issued leaf and its files.
#[derive(Clone)]
pub struct Leaf {
    pub certificate: CertificateDer<'static>,
    key: Arc<Vec<u8>>,
    pub certificate_path: PathBuf,
    pub key_path: PathBuf,
    /// Certificate followed by key, for servers that read one file.
    pub combined_path: PathBuf,
}

impl Leaf {
    pub fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.as_ref().clone()))
    }
}

struct Authority {
    certificate: Certificate,
    key: KeyPair,
}

/// The harness PKI.
pub struct Pki {
    directory: PathBuf,
    now: OffsetDateTime,
    test_ca: Authority,
    other_ca: Authority,
    /// The `.test` CA certificate.
    pub ca_path: PathBuf,
    /// The `.test` CA's CRL.
    pub crl_path: PathBuf,
    /// The unrelated CA's certificate, for a peer that must not trust the
    /// `.test` CA.
    pub other_ca_path: PathBuf,
    serial: std::sync::atomic::AtomicU64,
}

impl Pki {
    pub fn generate(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory)?;
        let now = OffsetDateTime::now_utc();
        let test_ca = authority("Waddle federation harness .test CA", now)?;
        let other_ca = authority("Untrusted harness CA", now)?;
        let crl = CertificateRevocationListParams {
            this_update: now - Duration::hours(1),
            next_update: now + Duration::days(6) - Duration::hours(1),
            crl_number: SerialNumber::from(1u64),
            issuing_distribution_point: None,
            revoked_certs: Vec::new(),
            key_identifier_method: KeyIdMethod::Sha256,
        }
        .signed_by(&test_ca.certificate, &test_ca.key)
        .context("sign the .test CRL")?;
        let ca_path = write(directory, "test-ca.pem", &test_ca.certificate.pem())?;
        let crl_path = write(directory, "test-ca.crl.pem", &crl.pem()?)?;
        let other_ca_path = write(directory, "other-ca.pem", &other_ca.certificate.pem())?;
        Ok(Self {
            directory: directory.to_owned(),
            now,
            test_ca,
            other_ca,
            ca_path,
            crl_path,
            other_ca_path,
            serial: std::sync::atomic::AtomicU64::new(100),
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Issue a leaf and write its PEM files.
    pub fn issue(&self, spec: &LeafSpec) -> Result<Leaf> {
        let mut params = CertificateParams::new(spec.names.clone())
            .with_context(|| format!("leaf names for {}", spec.stem))?;
        params.distinguished_name = common_name(&spec.names.join(" "));
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = match spec.usage {
            Usage::ServerAndClient => vec![
                ExtendedKeyUsagePurpose::ServerAuth,
                ExtendedKeyUsagePurpose::ClientAuth,
            ],
            Usage::ServerOnly => vec![ExtendedKeyUsagePurpose::ServerAuth],
        };
        let (not_before, not_after) = match spec.validity {
            Validity::Current => (self.now - Duration::hours(1), self.now + Duration::days(7)),
            Validity::Expired => (self.now - Duration::days(8), self.now - Duration::days(1)),
        };
        params.not_before = not_before;
        params.not_after = not_after;
        params.use_authority_key_identifier_extension = true;
        let serial = self
            .serial
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        params.serial_number = Some(SerialNumber::from(serial));
        let issuer = match spec.issuer {
            Issuer::TestCa => &self.test_ca,
            Issuer::Other => &self.other_ca,
        };
        let key = KeyPair::generate().context("leaf key")?;
        let certificate = params
            .signed_by(&key, &issuer.certificate, &issuer.key)
            .with_context(|| format!("sign leaf {}", spec.stem))?;
        let certificate_pem = certificate.pem();
        let key_pem = key.serialize_pem();
        let certificate_path = write(
            &self.directory,
            &[spec.stem.as_str(), "crt"].join("."),
            &certificate_pem,
        )?;
        let key_path = write_private(
            &self.directory,
            &[spec.stem.as_str(), "key"].join("."),
            &key_pem,
        )?;
        let combined_path = write_private(
            &self.directory,
            &[spec.stem.as_str(), "pem"].join("."),
            &[certificate_pem.as_str(), key_pem.as_str()].concat(),
        )?;
        Ok(Leaf {
            certificate: certificate.der().clone(),
            key: Arc::new(key.serialize_der()),
            certificate_path,
            key_path,
            combined_path,
        })
    }

    fn roots(&self) -> Result<RootCertStore> {
        let mut roots = RootCertStore::empty();
        roots.add(self.test_ca.certificate.der().clone())?;
        Ok(roots)
    }

    /// Client settings for the harness acting as an initiating server or
    /// as a WSS client: the `.test` CA as the only root, `leaf` as the
    /// client certificate when given, and only `versions`.
    pub fn client_config(
        &self,
        leaf: Option<&Leaf>,
        versions: &[&'static SupportedProtocolVersion],
    ) -> Result<ClientConfig> {
        let builder = ClientConfig::builder_with_provider(Arc::new(default_provider()))
            .with_protocol_versions(versions)?
            .with_root_certificates(self.roots()?);
        let config = match leaf {
            Some(leaf) => {
                builder.with_client_auth_cert(vec![leaf.certificate.clone()], leaf.key())?
            }
            None => builder.with_no_client_auth(),
        };
        Ok(config)
    }

    /// Server settings for the harness acting as a receiving server or as
    /// a TLS front: `leaf` as the server certificate and, when
    /// `request_client_certificate`, an optional client certificate
    /// checked against the `.test` CA.
    pub fn server_config(
        &self,
        leaf: &Leaf,
        request_client_certificate: bool,
        versions: &[&'static SupportedProtocolVersion],
    ) -> Result<ServerConfig> {
        let provider = Arc::new(default_provider());
        let builder = ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(versions)?;
        let builder = if request_client_certificate {
            let verifier =
                WebPkiClientVerifier::builder_with_provider(Arc::new(self.roots()?), provider)
                    .allow_unauthenticated()
                    .build()?;
            builder.with_client_cert_verifier(verifier)
        } else {
            builder.with_no_client_auth()
        };
        Ok(builder.with_single_cert(vec![leaf.certificate.clone()], leaf.key())?)
    }
}

/// Client settings for a client of a third-party server's C2S port. The
/// rows that make Prosody or ejabberd present a bad certificate swap the
/// certificate of the whole host, C2S included, while what they test is
/// S2S; the harness's own client therefore checks only the handshake
/// signatures there, never the chain. S2S verification is never relaxed.
pub fn c2s_client_config_for_third_party() -> Result<ClientConfig> {
    let provider = Arc::new(default_provider());
    Ok(ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&all_versions())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SignaturesOnly { provider }))
        .with_no_client_auth())
}

#[derive(Debug)]
struct SignaturesOnly {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for SignaturesOnly {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Both TLS versions the G1 profile allows.
pub fn all_versions() -> Vec<&'static SupportedProtocolVersion> {
    vec![&rustls::version::TLS13, &rustls::version::TLS12]
}

fn authority(name: &str, now: OffsetDateTime) -> Result<Authority> {
    let mut params = CertificateParams::default();
    params.distinguished_name = common_name(name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    params.not_before = now - Duration::hours(1);
    params.not_after = now + Duration::days(30);
    params.use_authority_key_identifier_extension = true;
    let key = KeyPair::generate().context("CA key")?;
    let certificate = params.self_signed(&key).context("self-sign CA")?;
    Ok(Authority { certificate, key })
}

fn common_name(name: &str) -> DistinguishedName {
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, name);
    distinguished_name
}

/// Write a file holding a private key, readable by its owner only.
fn write_private(directory: &Path, file: &str, contents: &str) -> Result<PathBuf> {
    let path = directory.join(file);
    crate::process::write_private(&path, contents.as_bytes())?;
    Ok(path)
}

fn write(directory: &Path, file: &str, contents: &str) -> Result<PathBuf> {
    let path = directory.join(file);
    std::fs::write(&path, contents).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}
