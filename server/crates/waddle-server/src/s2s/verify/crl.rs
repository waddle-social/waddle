//! Startup validation of the CRLs of a private-CA profile.
//!
//! The profile accepts only complete, direct CRLs: one per configured CA,
//! signed by that CA, current at load time. webpki, which checks revocation
//! while it builds a path, discards `thisUpdate`, consults the first CRL
//! whose issuer matches and ignores expiry unless told otherwise, so those
//! properties are established here before a CRL is handed to it.

use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::CertificateDer;
use webpki::{CertRevocationList, OwnedCertRevocationList};
use x509_parser::prelude::FromDer;
use x509_parser::revocation_list::CertificateRevocationList;

use super::der::{crl_entry_count, single, Reader, SEQUENCE};
use super::error::TrustErrorKind;
use super::limits::{MAX_CRL_DER_BYTES, MAX_CRL_ENTRIES};
use super::signature::{verify_signed_by, SignatureFailure};

/// id-ce-cRLNumber, 2.5.29.20.
const OID_CRL_NUMBER: &[u8] = &[0x55, 0x1d, 0x14];
/// id-ce-deltaCRLIndicator, 2.5.29.27.
const OID_DELTA_CRL_INDICATOR: &[u8] = &[0x55, 0x1d, 0x1b];
/// id-ce-issuingDistributionPoint, 2.5.29.28.
const OID_ISSUING_DISTRIBUTION_POINT: &[u8] = &[0x55, 0x1d, 0x1c];
/// id-ce-authorityKeyIdentifier, 2.5.29.35.
const OID_AUTHORITY_KEY_IDENTIFIER: &[u8] = &[0x55, 0x1d, 0x23];
/// `keyIdentifier [0]` of an AuthorityKeyIdentifier.
const AUTHORITY_KEY_IDENTIFIER_KEY: u8 = 0x80;
/// `indirectCRL [4]` of an IssuingDistributionPoint.
const DISTRIBUTION_POINT_INDIRECT: u8 = 0x84;
/// The encoded value of CRL version 2.
const CRL_VERSION_2: u32 = 1;

/// A configured CA as a CRL issuer.
pub(super) struct CrlIssuer<'a> {
    pub(super) certificate: &'a CertificateDer<'static>,
    /// The DER of the CA's subject name, tag and length included.
    pub(super) subject: &'a [u8],
    pub(super) subject_key_identifier: Option<&'a [u8]>,
}

/// A CRL that passed every startup check.
pub(super) struct LoadedCrl {
    /// The position of its issuer among the issuers given to [`load_crl`].
    pub(super) issuer: usize,
    pub(super) this_update: i64,
    pub(super) next_update: i64,
    pub(super) list: CertRevocationList<'static>,
}

/// Validate one CRL against the configured CAs at `now`.
pub(super) fn load_crl(
    der: &[u8],
    issuers: &[CrlIssuer<'_>],
    algorithms: &WebPkiSupportedAlgorithms,
    now: i64,
) -> Result<LoadedCrl, TrustErrorKind> {
    if der.len() > MAX_CRL_DER_BYTES {
        return Err(TrustErrorKind::CrlTooLarge);
    }
    // Bound the entry count on the raw bytes, before any parser allocates
    // per entry.
    let entries = crl_entry_count(der).map_err(|_| TrustErrorKind::MalformedCrl)?;
    if entries > MAX_CRL_ENTRIES {
        return Err(TrustErrorKind::TooManyCrlEntries);
    }

    let (rest, crl) =
        CertificateRevocationList::from_der(der).map_err(|_| TrustErrorKind::MalformedCrl)?;
    if !rest.is_empty() {
        return Err(TrustErrorKind::MalformedCrl);
    }
    if crl.version().map(|version| version.0) != Some(CRL_VERSION_2) {
        return Err(TrustErrorKind::CrlVersion);
    }

    let mut authority_key_identifier = None;
    let extensions = crl.extensions();
    for (index, extension) in extensions.iter().enumerate() {
        let identifier = extension.oid.as_bytes();
        if extensions[..index]
            .iter()
            .any(|earlier| earlier.oid.as_bytes() == identifier)
        {
            return Err(TrustErrorKind::MalformedCrl);
        }
        match identifier {
            OID_CRL_NUMBER => {}
            OID_AUTHORITY_KEY_IDENTIFIER => {
                authority_key_identifier = key_identifier(extension.value)?;
            }
            OID_DELTA_CRL_INDICATOR => return Err(TrustErrorKind::DeltaCrl),
            // Every issuing distribution point narrows the CRL's scope, so
            // none is a complete CRL for its issuer.
            OID_ISSUING_DISTRIBUTION_POINT => {
                return Err(distribution_point_kind(extension.value)?);
            }
            _ if extension.critical => return Err(TrustErrorKind::CrlCriticalExtension),
            _ => {}
        }
    }

    let this_update = crl.last_update().timestamp();
    let next_update = crl
        .next_update()
        .map(|time| time.timestamp())
        .ok_or(TrustErrorKind::CrlMissingNextUpdate)?;
    if next_update <= this_update {
        return Err(TrustErrorKind::CrlTimestampsReversed);
    }
    if now < this_update {
        return Err(TrustErrorKind::CrlNotYetValid);
    }
    if now >= next_update {
        return Err(TrustErrorKind::CrlExpired);
    }

    let issuer_name = crl.issuer().as_raw();
    let issuer = issuers
        .iter()
        .position(|issuer| issuer.subject == issuer_name)
        .ok_or(TrustErrorKind::CrlUnknownIssuer)?;
    if let (Some(authority), Some(subject)) = (
        authority_key_identifier,
        issuers[issuer].subject_key_identifier,
    ) {
        if authority != subject {
            return Err(TrustErrorKind::KeyIdentifierMismatch);
        }
    }

    if crl.tbs_cert_list.signature != crl.signature_algorithm {
        return Err(TrustErrorKind::SignatureAlgorithmMismatch);
    }
    verify_signed_by(issuers[issuer].certificate, der, algorithms).map_err(
        |failure| match failure {
            SignatureFailure::Malformed => TrustErrorKind::MalformedCrl,
            SignatureFailure::UnsupportedAlgorithm => TrustErrorKind::UnsupportedSignatureAlgorithm,
            SignatureFailure::Invalid => TrustErrorKind::CrlBadSignature,
        },
    )?;

    // webpki's own parse rejects what it cannot process in the entries,
    // such as a certificateIssuer entry extension, and indexes every serial.
    let list = OwnedCertRevocationList::from_der(der).map_err(|error| match error {
        webpki::Error::UnsupportedDeltaCrl => TrustErrorKind::DeltaCrl,
        webpki::Error::UnsupportedIndirectCrl => TrustErrorKind::IndirectCrl,
        webpki::Error::UnsupportedCrlIssuingDistributionPoint => {
            TrustErrorKind::CrlDistributionPoint
        }
        webpki::Error::UnsupportedCriticalExtension => TrustErrorKind::CrlCriticalExtension,
        webpki::Error::UnsupportedCrlVersion => TrustErrorKind::CrlVersion,
        _ => TrustErrorKind::MalformedCrl,
    })?;

    Ok(LoadedCrl {
        issuer,
        this_update,
        next_update,
        list: CertRevocationList::from(list),
    })
}

/// The key identifier of an AuthorityKeyIdentifier extension value, if it
/// carries one.
fn key_identifier(value: &[u8]) -> Result<Option<&[u8]>, TrustErrorKind> {
    let contents = single(value, SEQUENCE).map_err(|_| TrustErrorKind::MalformedCrl)?;
    let mut fields = Reader::new(contents);
    if fields.peek_tag() != Some(AUTHORITY_KEY_IDENTIFIER_KEY) {
        return Ok(None);
    }
    let key = fields.read().map_err(|_| TrustErrorKind::MalformedCrl)?;
    Ok(Some(key.contents))
}

/// Why a CRL with an issuing distribution point is refused: as an indirect
/// CRL when it says so, otherwise as a partitioned one.
fn distribution_point_kind(value: &[u8]) -> Result<TrustErrorKind, TrustErrorKind> {
    let contents = single(value, SEQUENCE).map_err(|_| TrustErrorKind::MalformedCrl)?;
    let mut fields = Reader::new(contents);
    while !fields.is_empty() {
        let field = fields.read().map_err(|_| TrustErrorKind::MalformedCrl)?;
        if field.tag == DISTRIBUTION_POINT_INDIRECT && field.contents != [0x00] {
            return Ok(TrustErrorKind::IndirectCrl);
        }
    }
    Ok(TrustErrorKind::CrlDistributionPoint)
}
