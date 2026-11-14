//! A test PKI generated in memory: CAs and end-entity certificates from
//! `rcgen`, and hand-encoded CRLs and extension values for the forms `rcgen`
//! cannot produce. Nothing is read from disk and no key leaves the process.

use std::sync::Arc;

use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use rcgen::{
    date_time_ymd, BasicConstraints, Certificate, CertificateParams, CustomExtension,
    DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{
    CertificateDer, CertificateRevocationListDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime,
};
use rustls::sign::{CertifiedKey, SigningKey as TlsSigningKey};
use x509_parser::certificate::X509Certificate;
use x509_parser::prelude::FromDer;

/// A calendar day at midnight UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day(pub i32, pub u8, pub u8);

/// The first day test CAs are valid.
pub const CA_FROM: Day = Day(2030, 1, 1);
/// The day test CAs expire.
pub const CA_UNTIL: Day = Day(2031, 1, 1);
/// The first day test end-entity certificates and CRLs are valid.
pub const VALID_FROM: Day = Day(2030, 6, 1);
/// The verification time of most cases.
pub const NOW: Day = Day(2030, 6, 15);
/// The day test end-entity certificates and CRLs expire.
pub const VALID_UNTIL: Day = Day(2030, 7, 1);

const INTEGER: u8 = 0x02;
const BIT_STRING: u8 = 0x03;
const OCTET_STRING: u8 = 0x04;
const OID: u8 = 0x06;
const UTF8_STRING: u8 = 0x0c;
const IA5_STRING: u8 = 0x16;
const UTC_TIME: u8 = 0x17;
const SEQUENCE: u8 = 0x30;
const BOOLEAN_TRUE: [u8; 3] = [0x01, 0x01, 0xff];

/// id-on-xmppAddr, 1.3.6.1.5.5.7.8.5.
pub const OID_XMPP_ADDR: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x08, 0x05];
/// id-on-dnsSRV, 1.3.6.1.5.5.7.8.7.
pub const OID_SRV_NAME: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x08, 0x07];
/// An OID no verifier knows: 1.3.6.1.4.1.99999.1.
const OID_UNKNOWN: &[u64] = &[1, 3, 6, 1, 4, 1, 99999, 1];
const OID_UNKNOWN_DER: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x86, 0x8d, 0x1f, 0x01];
/// ecdsa-with-SHA256 as an `AlgorithmIdentifier`.
const ECDSA_SHA256: &[u8] = &[
    0x30, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02,
];

/// The instant `day` begins.
pub fn unix(day: Day) -> UnixTime {
    let seconds = date_time_ymd(day.0, day.1, day.2).unix_timestamp();
    UnixTime::since_unix_epoch(std::time::Duration::from_secs(
        u64::try_from(seconds).expect("test days are after the epoch"),
    ))
}

/// Encode one DER element.
pub fn tlv(tag: u8, contents: &[u8]) -> Vec<u8> {
    let mut encoded = vec![tag];
    let length = contents.len();
    if length < 0x80 {
        encoded.push(u8::try_from(length).expect("short length"));
    } else {
        let digits = length.to_be_bytes();
        let digits = &digits[digits.iter().take_while(|digit| **digit == 0).count()..];
        encoded.push(0x80 | u8::try_from(digits.len()).expect("length of a length"));
        encoded.extend_from_slice(digits);
    }
    encoded.extend_from_slice(contents);
    encoded
}

fn sequence(parts: &[&[u8]]) -> Vec<u8> {
    tlv(SEQUENCE, &parts.concat())
}

fn utc_time(day: Day) -> Vec<u8> {
    let text = format!("{:02}{:02}{:02}000000Z", day.0 % 100, day.1, day.2);
    tlv(UTC_TIME, text.as_bytes())
}

/// A dNSName subjectAltName entry.
pub fn san_dns(name: &str) -> Vec<u8> {
    tlv(0x82, name.as_bytes())
}

/// An otherName subjectAltName entry whose value is `value`, which the
/// caller encodes, wrapped in the `[0] EXPLICIT` tag.
pub fn san_other_name(type_id: &[u8], value: &[u8]) -> Vec<u8> {
    tlv(0xa0, &[tlv(OID, type_id), tlv(0xa0, value)].concat())
}

/// A correctly encoded SRV-ID entry.
pub fn san_srv(name: &str) -> Vec<u8> {
    san_other_name(OID_SRV_NAME, &tlv(IA5_STRING, name.as_bytes()))
}

/// A correctly encoded XmppAddr entry.
pub fn san_xmpp_addr(addr: &str) -> Vec<u8> {
    san_other_name(OID_XMPP_ADDR, &tlv(UTF8_STRING, addr.as_bytes()))
}

/// A subjectAltName extension with exactly `entries`.
pub fn san_extension(entries: &[Vec<u8>]) -> CustomExtension {
    CustomExtension::from_oid_content(&[2, 5, 29, 17], tlv(SEQUENCE, &entries.concat()))
}

/// A critical nameConstraints extension that permits one otherName subtree,
/// an SRVName, which the verifier cannot enforce.
pub fn other_name_constraint(srv_name: &str) -> CustomExtension {
    let base = san_srv(srv_name);
    let permitted = tlv(0xa0, &tlv(SEQUENCE, &base));
    let mut extension =
        CustomExtension::from_oid_content(&[2, 5, 29, 30], tlv(SEQUENCE, &permitted));
    extension.set_criticality(true);
    extension
}

/// A critical extension no verifier processes.
pub fn unknown_critical_extension() -> CustomExtension {
    let mut extension = CustomExtension::from_oid_content(OID_UNKNOWN, vec![0x05, 0x00]);
    extension.set_criticality(true);
    extension
}

/// A non-critical extension with `bytes` bytes of content under a private
/// OID ending in `arc`.
pub fn padding_extension(arc: u64, bytes: usize) -> CustomExtension {
    CustomExtension::from_oid_content(
        &[1, 3, 6, 1, 4, 1, 99999, 2, arc],
        tlv(OCTET_STRING, &vec![0u8; bytes]),
    )
}

fn common_name(name: &str) -> DistinguishedName {
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, name);
    distinguished_name
}

/// Parameters of a CA certificate that meets the issuer profile.
pub fn ca_params(name: &str) -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name = common_name(name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    params.not_before = date_time_ymd(CA_FROM.0, CA_FROM.1, CA_FROM.2);
    params.not_after = date_time_ymd(CA_UNTIL.0, CA_UNTIL.1, CA_UNTIL.2);
    params.use_authority_key_identifier_extension = true;
    params
}

/// Parameters of an end-entity certificate that meets the leaf profile and
/// carries `dns_names` as DNS-IDs.
pub fn leaf_params(dns_names: &[&str]) -> CertificateParams {
    let names: Vec<String> = dns_names.iter().map(|name| (*name).to_owned()).collect();
    let mut params = CertificateParams::new(names).expect("DNS names are IA5 strings");
    params.distinguished_name = common_name("s2s test server");
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    params.not_before = date_time_ymd(VALID_FROM.0, VALID_FROM.1, VALID_FROM.2);
    params.not_after = date_time_ymd(VALID_UNTIL.0, VALID_UNTIL.1, VALID_UNTIL.2);
    params.use_authority_key_identifier_extension = true;
    params
}

/// Set the validity period of `params`.
pub fn valid_between(params: &mut CertificateParams, from: Day, until: Day) {
    params.not_before = date_time_ymd(from.0, from.1, from.2);
    params.not_after = date_time_ymd(until.0, until.1, until.2);
}

/// A certificate with the last byte of its signature changed.
pub fn corrupt_signature(certificate: &CertificateDer<'static>) -> CertificateDer<'static> {
    let mut der = certificate.to_vec();
    let last = der.last_mut().expect("a certificate is not empty");
    *last ^= 0x01;
    CertificateDer::from(der)
}

/// The DER of the subject name of `certificate`, tag and length included.
fn subject_of(certificate: &CertificateDer<'_>) -> Vec<u8> {
    let (_, parsed) = X509Certificate::from_der(certificate).expect("rcgen emits valid DER");
    parsed.subject().as_raw().to_vec()
}

/// The serial number of `certificate` as it is encoded.
pub fn serial_of(certificate: &CertificateDer<'_>) -> Vec<u8> {
    let (_, parsed) = X509Certificate::from_der(certificate).expect("rcgen emits valid DER");
    parsed.raw_serial().to_vec()
}

/// What a hand-encoded CRL contains.
#[derive(Debug, Clone)]
pub struct CrlSpec {
    pub this_update: Day,
    pub next_update: Option<Day>,
    /// Serial numbers of revoked certificates, as encoded.
    pub revoked: Vec<Vec<u8>>,
    /// Further CRL extensions, each a complete `Extension`.
    pub extensions: Vec<Vec<u8>>,
    /// Overrides the authority key identifier, which otherwise names the
    /// issuer's key.
    pub authority_key_identifier: Option<Vec<u8>>,
}

impl CrlSpec {
    /// A complete CRL that is current at [`NOW`] and revokes nothing.
    pub fn current() -> Self {
        Self {
            this_update: VALID_FROM,
            next_update: Some(VALID_UNTIL),
            revoked: Vec::new(),
            extensions: Vec::new(),
            authority_key_identifier: None,
        }
    }

    /// The same CRL revoking `certificates`.
    pub fn revoking(certificates: &[&CertificateDer<'_>]) -> Self {
        Self {
            revoked: certificates
                .iter()
                .map(|certificate| serial_of(certificate))
                .collect(),
            ..Self::current()
        }
    }

    /// The same CRL with one more extension.
    pub fn with_extension(mut self, extension: Vec<u8>) -> Self {
        self.extensions.push(extension);
        self
    }
}

fn crl_extension(identifier: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
    let critical: &[u8] = if critical { &BOOLEAN_TRUE } else { &[] };
    sequence(&[&tlv(OID, identifier), critical, &tlv(OCTET_STRING, value)])
}

/// The critical deltaCRLIndicator extension (RFC 5280 §5.2.4).
pub fn delta_crl_indicator() -> Vec<u8> {
    crl_extension(&[0x55, 0x1d, 0x1b], true, &tlv(INTEGER, &[1]))
}

/// A critical issuingDistributionPoint that marks the CRL as indirect.
pub fn indirect_distribution_point() -> Vec<u8> {
    crl_extension(
        &[0x55, 0x1d, 0x1c],
        true,
        &tlv(SEQUENCE, &[0x84, 0x01, 0xff]),
    )
}

/// A critical issuingDistributionPoint that limits the CRL to end-entity
/// certificates.
pub fn user_certificates_distribution_point() -> Vec<u8> {
    crl_extension(
        &[0x55, 0x1d, 0x1c],
        true,
        &tlv(SEQUENCE, &[0x81, 0x01, 0xff]),
    )
}

/// A critical CRL extension no verifier processes.
pub fn unknown_critical_crl_extension() -> Vec<u8> {
    crl_extension(OID_UNKNOWN_DER, true, &[0x05, 0x00])
}

/// A non-critical CRL extension of slightly more than one mebibyte.
pub fn oversized_extension() -> Vec<u8> {
    crl_extension(
        OID_UNKNOWN_DER,
        false,
        &tlv(OCTET_STRING, &vec![0u8; 1024 * 1024]),
    )
}

/// A certification authority: its certificate and private key.
pub struct Authority {
    pub certificate: Certificate,
    pub key: KeyPair,
}

impl Authority {
    /// A self-signed root named `name`.
    pub fn root(name: &str) -> Self {
        Self::root_with(ca_params(name))
    }

    /// A self-signed root with `params`.
    pub fn root_with(params: CertificateParams) -> Self {
        let key = KeyPair::generate().expect("generate a CA key");
        let certificate = params.self_signed(&key).expect("self-sign a CA");
        Self { certificate, key }
    }

    /// An intermediate CA named `name`, issued by this CA.
    pub fn intermediate(&self, name: &str) -> Self {
        self.intermediate_with(ca_params(name))
    }

    /// An intermediate CA with `params`, issued by this CA.
    pub fn intermediate_with(&self, params: CertificateParams) -> Self {
        let key = KeyPair::generate().expect("generate a CA key");
        let certificate = params
            .signed_by(&key, &self.certificate, &self.key)
            .expect("issue an intermediate");
        Self { certificate, key }
    }

    /// A CA certificate for the subject and key of `subject`, issued by
    /// this CA: a cross-certificate.
    pub fn cross_sign(&self, subject: &Authority) -> CertificateDer<'static> {
        subject
            .certificate
            .params()
            .clone()
            .signed_by(&subject.key, &self.certificate, &self.key)
            .expect("cross-sign a CA")
            .der()
            .clone()
    }

    /// This CA's certificate.
    pub fn der(&self) -> CertificateDer<'static> {
        self.certificate.der().clone()
    }

    /// An end-entity certificate with `params` and a fresh key.
    pub fn issue(&self, params: CertificateParams) -> Credential {
        let key = KeyPair::generate().expect("generate a leaf key");
        let certificate = params
            .signed_by(&key, &self.certificate, &self.key)
            .expect("issue a leaf");
        Credential {
            chain: vec![certificate.der().clone()],
            key,
        }
    }

    /// A credential for `dns_names` that meets the leaf profile.
    pub fn issue_for(&self, dns_names: &[&str]) -> Credential {
        self.issue(leaf_params(dns_names))
    }

    /// A complete, current CRL of this CA that revokes nothing.
    pub fn clean_crl(&self) -> CertificateRevocationListDer<'static> {
        self.crl(&CrlSpec::current())
    }

    /// A CRL of this CA as `spec` describes, signed with this CA's key.
    pub fn crl(&self, spec: &CrlSpec) -> CertificateRevocationListDer<'static> {
        // The identifier `rcgen` derives for this CA's key, which is also
        // the subject key identifier of its certificate.
        let key_identifier = spec
            .authority_key_identifier
            .clone()
            .unwrap_or_else(|| self.certificate.key_identifier());

        let mut fields = Vec::new();
        // Version 2.
        fields.extend(tlv(INTEGER, &[1]));
        fields.extend_from_slice(ECDSA_SHA256);
        fields.extend(subject_of(self.certificate.der()));
        fields.extend(utc_time(spec.this_update));
        if let Some(next_update) = spec.next_update {
            fields.extend(utc_time(next_update));
        }
        if !spec.revoked.is_empty() {
            let entries: Vec<u8> = spec
                .revoked
                .iter()
                .flat_map(|serial| sequence(&[&tlv(INTEGER, serial), &utc_time(spec.this_update)]))
                .collect();
            fields.extend(tlv(SEQUENCE, &entries));
        }
        let mut extensions = Vec::new();
        extensions.extend(crl_extension(
            &[0x55, 0x1d, 0x23],
            false,
            &tlv(SEQUENCE, &tlv(0x80, &key_identifier)),
        ));
        extensions.extend(crl_extension(
            &[0x55, 0x1d, 0x14],
            false,
            &tlv(INTEGER, &[1]),
        ));
        for extension in &spec.extensions {
            extensions.extend_from_slice(extension);
        }
        fields.extend(tlv(0xa0, &tlv(SEQUENCE, &extensions)));

        let signed = tlv(SEQUENCE, &fields);
        let signing_key = SigningKey::from_pkcs8_der(&self.key.serialize_der())
            .expect("an rcgen P-256 key is PKCS#8");
        let signature: Signature = signing_key.sign(&signed);
        let mut signature_bits = vec![0u8];
        signature_bits.extend_from_slice(signature.to_der().as_bytes());
        CertificateRevocationListDer::from(sequence(&[
            &signed,
            ECDSA_SHA256,
            &tlv(BIT_STRING, &signature_bits),
        ]))
    }
}

/// A CRL with the last byte of its signature changed.
pub fn corrupt_crl_signature(
    crl: &CertificateRevocationListDer<'static>,
) -> CertificateRevocationListDer<'static> {
    let mut der = crl.to_vec();
    let last = der.last_mut().expect("a CRL is not empty");
    *last ^= 0x01;
    CertificateRevocationListDer::from(der)
}

/// A certificate chain and the private key of its end-entity certificate.
pub struct Credential {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: KeyPair,
}

impl Credential {
    /// The end-entity certificate.
    pub fn leaf(&self) -> &CertificateDer<'static> {
        &self.chain[0]
    }

    /// The same credential presenting `certificates` after its end-entity
    /// certificate.
    pub fn presenting(mut self, certificates: &[CertificateDer<'static>]) -> Self {
        self.chain.extend_from_slice(certificates);
        self
    }

    /// The same credential with its end-entity certificate replaced.
    pub fn with_leaf(mut self, leaf: CertificateDer<'static>) -> Self {
        self.chain[0] = leaf;
        self
    }

    /// The private key as rustls signs with it.
    pub fn signing_key(&self) -> Arc<dyn TlsSigningKey> {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.serialize_der()));
        default_provider()
            .key_provider
            .load_private_key(key)
            .expect("load a test key")
    }

    /// The chain and key as rustls presents them.
    pub fn certified_key(&self) -> Arc<CertifiedKey> {
        Arc::new(CertifiedKey::new(self.chain.clone(), self.signing_key()))
    }
}
