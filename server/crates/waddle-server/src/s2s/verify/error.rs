//! Typed failures of the S2S certificate verifier.
//!
//! The errors name the check that failed. A [`VerifyError::Path`] failure
//! keeps webpki's error, whose `Display` and `Debug` output can include
//! values taken from the peer's certificates, such as extended key usage
//! OIDs or a signature algorithm identifier. Logs therefore record
//! [`VerifyError::code`], which is a fixed string, and never format a
//! [`VerifyError`] itself. A peer only ever learns the TLS alert that
//! [`VerifyError::into_rustls`] maps a failure to.

use std::fmt;
use std::sync::Arc;

use rustls::{CertificateError, OtherError};
use thiserror::Error;

/// Why a peer certificate chain was refused.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum VerifyError {
    /// More certificates than [`super::limits::MAX_CHAIN_CERTIFICATES`].
    #[error("the presented chain has too many certificates")]
    ChainTooLong,
    /// More bytes than [`super::limits::MAX_CHAIN_DER_BYTES`].
    #[error("the presented chain is too large")]
    ChainTooLarge,
    /// One certificate exceeds [`super::limits::MAX_CERTIFICATE_DER_BYTES`].
    #[error("a presented certificate is too large")]
    CertificateTooLarge,
    /// A presented certificate is not one strictly encoded X.509 v3
    /// certificate, has trailing bytes, or repeats an extension.
    #[error("a presented certificate is malformed")]
    MalformedCertificate,
    /// More extensions than [`super::limits::MAX_CERTIFICATE_EXTENSIONS`].
    #[error("a presented certificate has too many extensions")]
    TooManyExtensions,
    /// More names or name-constraint subtrees than the verifier parses.
    #[error("a presented certificate has too many names")]
    TooManyNames,
    /// The private-CA profile's certificates or CRLs are not valid yet at
    /// the verification time.
    #[error("the trust profile is not valid yet")]
    TrustNotYetValid,
    /// A CA certificate or CRL of the private-CA profile has expired at the
    /// verification time.
    #[error("the trust profile has expired")]
    TrustExpired,
    /// Path building, signature, validity, name-constraint or revocation
    /// checking refused the chain.
    #[error("certificate path validation failed: {0}")]
    Path(webpki::Error),
    /// The end-entity certificate asserts the CA basic constraint.
    #[error("the end-entity certificate is a CA certificate")]
    LeafIsCa,
    /// The private-CA profile requires a keyUsage extension on the
    /// end-entity certificate.
    #[error("the end-entity certificate has no key usage extension")]
    MissingKeyUsage,
    /// The end-entity certificate's keyUsage lacks digitalSignature.
    #[error("the end-entity certificate may not be used for digital signatures")]
    KeyUsage,
    /// The end-entity certificate has no extended key usage extension.
    #[error("the end-entity certificate has no extended key usage extension")]
    MissingExtendedKeyUsage,
    /// The end-entity certificate's extended key usage does not permit its
    /// TLS role.
    #[error("the end-entity certificate's extended key usage does not permit this role")]
    ExtendedKeyUsage,
    /// The end-entity certificate is valid for longer than the
    /// public-WebPKI profile accepts.
    #[error("the end-entity certificate's validity period is too long")]
    ValidityTooLong,
    /// An issuer in the path is not a CA or may not sign certificates.
    #[error("an issuer in the path may not sign certificates")]
    IssuerUsage,
    /// A CA in the path constrains the otherName form, which the verifier
    /// cannot enforce.
    #[error("the path carries a name constraint the verifier cannot enforce")]
    UnsupportedNameConstraint,
    /// An SRV-ID or XmppAddr entry of the end-entity certificate is not
    /// correctly encoded.
    #[error("the end-entity certificate carries a malformed XMPP identifier")]
    MalformedIdentifier,
    /// The end-entity certificate repeats an SRV-ID or XmppAddr entry.
    #[error("the end-entity certificate repeats an XMPP identifier")]
    DuplicateIdentifier,
    /// No presented identifier matches the reference identity.
    #[error("the certificate does not name the reference domain")]
    IdentityMismatch,
    /// The TLS server name differs from the verifier's reference identity.
    #[error("the TLS server name is not the verifier's reference domain")]
    ReferenceMismatch,
    /// The verifier already verified a certificate; it serves one
    /// connection.
    #[error("the verifier was already used for a connection")]
    VerifierReused,
    /// The peer's handshake signature does not verify under the key of the
    /// certificate whose chain was verified, or it was presented for a
    /// certificate this verifier has not verified.
    #[error("the handshake signature does not prove possession of the verified certificate")]
    HandshakeSignature,
}

impl VerifyError {
    /// A stable code for logs. It never contains peer-supplied text.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ChainTooLong => "chain_too_long",
            Self::ChainTooLarge => "chain_too_large",
            Self::CertificateTooLarge => "certificate_too_large",
            Self::MalformedCertificate => "malformed_certificate",
            Self::TooManyExtensions => "too_many_extensions",
            Self::TooManyNames => "too_many_names",
            Self::TrustNotYetValid => "trust_not_yet_valid",
            Self::TrustExpired => "trust_expired",
            Self::Path(_) => "path",
            Self::LeafIsCa => "leaf_is_ca",
            Self::MissingKeyUsage => "missing_key_usage",
            Self::KeyUsage => "key_usage",
            Self::MissingExtendedKeyUsage => "missing_extended_key_usage",
            Self::ExtendedKeyUsage => "extended_key_usage",
            Self::ValidityTooLong => "validity_too_long",
            Self::IssuerUsage => "issuer_usage",
            Self::UnsupportedNameConstraint => "unsupported_name_constraint",
            Self::MalformedIdentifier => "malformed_identifier",
            Self::DuplicateIdentifier => "duplicate_identifier",
            Self::IdentityMismatch => "identity_mismatch",
            Self::ReferenceMismatch => "reference_mismatch",
            Self::VerifierReused => "verifier_reused",
            Self::HandshakeSignature => "handshake_signature",
        }
    }

    /// The rustls error that ends the handshake, which selects the TLS alert
    /// the peer receives.
    pub(super) fn into_rustls(self) -> rustls::Error {
        let certificate_error = match &self {
            Self::ChainTooLong
            | Self::ChainTooLarge
            | Self::CertificateTooLarge
            | Self::MalformedCertificate
            | Self::TooManyExtensions
            | Self::TooManyNames
            | Self::MalformedIdentifier
            | Self::DuplicateIdentifier => CertificateError::BadEncoding,
            Self::TrustNotYetValid | Self::TrustExpired => CertificateError::UnknownIssuer,
            Self::Path(error) => path_certificate_error(error),
            Self::LeafIsCa
            | Self::MissingKeyUsage
            | Self::KeyUsage
            | Self::MissingExtendedKeyUsage
            | Self::ExtendedKeyUsage
            | Self::IssuerUsage => CertificateError::InvalidPurpose,
            Self::IdentityMismatch => CertificateError::NotValidForName,
            Self::HandshakeSignature => CertificateError::BadSignature,
            Self::ValidityTooLong
            | Self::UnsupportedNameConstraint
            | Self::ReferenceMismatch
            | Self::VerifierReused => CertificateError::ApplicationVerificationFailure,
        };
        rustls::Error::InvalidCertificate(certificate_error)
    }
}

/// The rustls certificate error for a webpki path failure. Failures without
/// a dedicated rustls error keep the webpki error as their source.
fn path_certificate_error(error: &webpki::Error) -> CertificateError {
    match error {
        webpki::Error::BadDer
        | webpki::Error::BadDerTime
        | webpki::Error::TrailingData(_)
        | webpki::Error::UnsupportedCertVersion
        | webpki::Error::SignatureAlgorithmMismatch => CertificateError::BadEncoding,
        webpki::Error::CertNotValidYet { .. } => CertificateError::NotValidYet,
        webpki::Error::CertExpired { .. } | webpki::Error::InvalidCertValidity => {
            CertificateError::Expired
        }
        webpki::Error::UnknownIssuer => CertificateError::UnknownIssuer,
        webpki::Error::CertRevoked => CertificateError::Revoked,
        webpki::Error::UnknownRevocationStatus => CertificateError::UnknownRevocationStatus,
        webpki::Error::CrlExpired { .. } => CertificateError::ExpiredRevocationList,
        webpki::Error::InvalidSignatureForPublicKey
        | webpki::Error::InvalidCrlSignatureForPublicKey => CertificateError::BadSignature,
        webpki::Error::UnsupportedCriticalExtension => CertificateError::UnhandledCriticalExtension,
        webpki::Error::CaUsedAsEndEntity
        | webpki::Error::EndEntityUsedAsCa
        | webpki::Error::RequiredEkuNotFoundContext(_)
        | webpki::Error::EmptyEkuExtension
        | webpki::Error::IssuerNotCrlSigner => CertificateError::InvalidPurpose,
        other => CertificateError::Other(OtherError(Arc::new(other.clone()))),
    }
}

/// Which configured item a [`TrustError`] concerns, by its position in the
/// material handed to the loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustItem {
    /// A trust anchor.
    Anchor(usize),
    /// An intermediate CA certificate.
    Intermediate(usize),
    /// A CRL.
    Crl(usize),
}

impl fmt::Display for TrustItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anchor(index) => write!(formatter, "trust anchor {index}"),
            Self::Intermediate(index) => write!(formatter, "intermediate {index}"),
            Self::Crl(index) => write!(formatter, "CRL {index}"),
        }
    }
}

/// What is wrong with configured trust material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TrustErrorKind {
    #[error("no trust anchor is configured")]
    NoTrustAnchors,
    #[error("too many CA certificates are configured")]
    TooManyCaCertificates,
    #[error("too many public roots are configured")]
    TooManyPublicRoots,
    #[error("the certificate is too large")]
    CertificateTooLarge,
    #[error("the certificate is malformed")]
    MalformedCertificate,
    #[error("the certificate has too many extensions")]
    TooManyExtensions,
    #[error("the certificate has too many names")]
    TooManyNames,
    #[error("the certificate has a critical extension the verifier does not process")]
    UnsupportedCriticalExtension,
    #[error("the certificate is not a CA certificate")]
    NotCa,
    #[error("the CA certificate has no key usage extension")]
    MissingKeyUsage,
    #[error("the CA certificate may not sign certificates")]
    CertSignUsage,
    #[error("the CA certificate may not sign CRLs")]
    CrlSignUsage,
    #[error("the trust anchor's extended key usage does not include serverAuth")]
    AnchorExtendedKeyUsage,
    #[error("the intermediate's extended key usage admits neither TLS role")]
    IntermediateExtendedKeyUsage,
    #[error("the certificate is not valid yet")]
    NotYetValid,
    #[error("the certificate has expired")]
    Expired,
    #[error("the inner and outer signature algorithms differ")]
    SignatureAlgorithmMismatch,
    #[error("the signature algorithm is not one the TLS provider verifies")]
    UnsupportedSignatureAlgorithm,
    #[error("the signature does not verify")]
    BadSignature,
    #[error("the trust anchor is not self-issued")]
    AnchorNotSelfIssued,
    #[error("two CA certificates have the same subject")]
    DuplicateSubject,
    #[error("the issuer of the intermediate is not configured")]
    UnknownIssuer,
    #[error("the issuer chain of the intermediate does not reach a trust anchor")]
    IssuerCycle,
    #[error("the intermediate violates a path length constraint")]
    PathLength,
    #[error("the authority key identifier does not match the issuer")]
    KeyIdentifierMismatch,
    #[error("the CA certificate carries a name constraint the verifier cannot enforce")]
    UnsupportedNameConstraint,
    #[error("the intermediate is revoked by its issuer's CRL")]
    RevokedIntermediate,
    #[error("no CRL is configured")]
    NoCrls,
    #[error("too many CRLs are configured")]
    TooManyCrls,
    #[error("the CRL is too large")]
    CrlTooLarge,
    #[error("the CRL is malformed")]
    MalformedCrl,
    #[error("the CRL has too many entries")]
    TooManyCrlEntries,
    #[error("the CRL is not a version 2 CRL")]
    CrlVersion,
    #[error("the CRL is a delta CRL")]
    DeltaCrl,
    #[error("the CRL is an indirect CRL")]
    IndirectCrl,
    #[error("the CRL is limited by an issuing distribution point")]
    CrlDistributionPoint,
    #[error("the CRL has a critical extension the verifier does not process")]
    CrlCriticalExtension,
    #[error("the CRL has no nextUpdate")]
    CrlMissingNextUpdate,
    #[error("the CRL's nextUpdate is not after its thisUpdate")]
    CrlTimestampsReversed,
    #[error("the CRL's nextUpdate is further after its thisUpdate than the profile accepts")]
    CrlValidityTooLong,
    #[error("the CRL is not valid yet")]
    CrlNotYetValid,
    #[error("the CRL has expired")]
    CrlExpired,
    #[error("the issuer of the CRL is not configured")]
    CrlUnknownIssuer,
    #[error("the CRL signature does not verify")]
    CrlBadSignature,
    #[error("two CRLs have the same issuer")]
    DuplicateCrl,
    #[error("a configured CA has no CRL")]
    MissingCrl,
    #[error("the domain already has a private-CA profile")]
    DuplicateDomain,
    #[error("the domain cannot name a federation peer")]
    NotAPeerDomain,
}

/// Configured trust material was refused. Nothing is loaded from material
/// that fails any check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustError {
    kind: TrustErrorKind,
    item: Option<TrustItem>,
}

impl TrustError {
    pub(super) fn new(kind: TrustErrorKind) -> Self {
        Self { kind, item: None }
    }

    pub(super) fn at(kind: TrustErrorKind, item: TrustItem) -> Self {
        Self {
            kind,
            item: Some(item),
        }
    }

    /// What is wrong.
    pub fn kind(&self) -> TrustErrorKind {
        self.kind
    }

    /// The configured item that is wrong, when the failure concerns one.
    pub fn item(&self) -> Option<TrustItem> {
        self.item
    }
}

impl fmt::Display for TrustError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.item {
            Some(item) => write!(formatter, "{item}: {}", self.kind),
            None => write!(formatter, "{}", self.kind),
        }
    }
}

impl std::error::Error for TrustError {}

/// A TLS configuration could not be built.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum TlsConfigError {
    /// The crypto provider supports none of the profile's TLS versions.
    #[error("the crypto provider cannot serve the TLS profile: {0}")]
    Provider(rustls::Error),
    /// The private key does not belong to the certificate.
    #[error("the local certificate and private key do not match")]
    KeyMismatch,
    /// The local certificate chain is empty or unusable.
    #[error("the local certificate chain is unusable: {0}")]
    Credentials(rustls::Error),
}

/// Verifier workers could not be set up or could not take a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkerError {
    /// Every worker is occupied and no further caller may wait.
    #[error("every verifier worker is occupied")]
    Saturated,
    /// The operating system refused a worker thread.
    #[error("a verifier worker thread could not be started")]
    Spawn,
    /// More workers were requested than [`super::VerifierWorkers::MAX_LIMIT`].
    #[error("more verifier workers were requested than the runtime can count")]
    LimitTooLarge,
}

/// A worker ended without delivering its job's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the verifier worker ended without a result")]
pub struct WorkerLost;
