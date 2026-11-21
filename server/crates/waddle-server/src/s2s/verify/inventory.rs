//! Startup validation of a private-CA trust profile.
//!
//! A private-CA profile is an operator-provisioned set of trust anchors,
//! intermediate CA certificates and CRLs. Everything is validated once,
//! before any S2S socket exists, and the result is immutable: changing the
//! files does not change a running profile. Material that fails any check
//! loads nothing.

use std::fmt;

use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, CertificateRevocationListDer, TrustAnchor, UnixTime};
use webpki::{anchor_from_trusted_cert, CertRevocationList, EndEntityCert};

use super::crl::{load_crl, CrlIssuer, LoadedCrl};
use super::error::{TrustError, TrustErrorKind, TrustItem};
use super::facts::{CertificateFacts, FactsError};
use super::limits::{MAX_PROFILE_CA_CERTIFICATES, MAX_PROFILE_CRLS};
use super::signature::{verify_signed_by, SignatureFailure};

/// The public trust material of one private-CA profile, as DER.
#[derive(Debug, Clone, Default)]
pub struct PrivateCaMaterial {
    /// Self-signed CA certificates the operator trusts. Only these become
    /// trust anchors.
    pub anchors: Vec<CertificateDer<'static>>,
    /// CA certificates issued, directly or through each other, by the
    /// anchors. A peer's chain is built from these, never from
    /// intermediates the peer sends.
    pub intermediates: Vec<CertificateDer<'static>>,
    /// Exactly one complete CRL for every anchor and intermediate.
    pub crls: Vec<CertificateRevocationListDer<'static>>,
}

/// A validated, immutable private-CA trust profile.
pub struct PrivateCaProfile {
    pub(super) anchors: Vec<TrustAnchor<'static>>,
    pub(super) intermediates: Vec<CertificateDer<'static>>,
    pub(super) intermediate_facts: Vec<CertificateFacts>,
    pub(super) crls: Vec<CertRevocationList<'static>>,
    /// The first instant at which every certificate and CRL is valid.
    pub(super) valid_from: i64,
    /// The first instant at which a certificate or CRL is no longer valid.
    pub(super) expires_at: i64,
}

impl fmt::Debug for PrivateCaProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Counts only: the material itself can run to megabytes.
        formatter
            .debug_struct("PrivateCaProfile")
            .field("anchors", &self.anchors.len())
            .field("intermediates", &self.intermediates.len())
            .field("crls", &self.crls.len())
            .field("valid_from", &self.valid_from)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A CA certificate under validation.
struct Authority<'a> {
    item: TrustItem,
    certificate: &'a CertificateDer<'static>,
    facts: CertificateFacts,
    is_anchor: bool,
}

impl PrivateCaProfile {
    /// Validate `material` at `now` and build the profile.
    ///
    /// Signatures are verified with `algorithms`, the certificate signature
    /// algorithms of the TLS crypto provider, so the profile accepts exactly
    /// what a handshake can verify.
    pub fn load(
        material: &PrivateCaMaterial,
        algorithms: &WebPkiSupportedAlgorithms,
        now: UnixTime,
    ) -> Result<Self, TrustError> {
        let now = unix_seconds(now);
        if material.anchors.is_empty() {
            return Err(TrustError::new(TrustErrorKind::NoTrustAnchors));
        }
        if material.anchors.len() + material.intermediates.len() > MAX_PROFILE_CA_CERTIFICATES {
            return Err(TrustError::new(TrustErrorKind::TooManyCaCertificates));
        }
        if material.crls.is_empty() {
            return Err(TrustError::new(TrustErrorKind::NoCrls));
        }
        if material.crls.len() > MAX_PROFILE_CRLS {
            return Err(TrustError::new(TrustErrorKind::TooManyCrls));
        }

        let mut authorities = Vec::with_capacity(MAX_PROFILE_CA_CERTIFICATES);
        let anchors = material
            .anchors
            .iter()
            .enumerate()
            .map(|(index, certificate)| (TrustItem::Anchor(index), certificate, true));
        let intermediates = material
            .intermediates
            .iter()
            .enumerate()
            .map(|(index, certificate)| (TrustItem::Intermediate(index), certificate, false));
        for (item, certificate, is_anchor) in anchors.chain(intermediates) {
            let facts =
                read_authority(certificate, now).map_err(|kind| TrustError::at(kind, item))?;
            // webpki applies the extended key usage of the intermediates in
            // a path but not of its trust anchor. An anchor that carries the
            // extension must therefore admit both TLS roles here, which
            // means serverAuth: a profile serves either role.
            if is_anchor
                && facts
                    .extended_key_usage
                    .is_some_and(|usage| !usage.server_auth)
            {
                return Err(TrustError::at(TrustErrorKind::AnchorExtendedKeyUsage, item));
            }
            // webpki applies an intermediate's extended key usage to every
            // path through it. On a private-CA profile the receiving role
            // needs serverAuth and the initiating role accepts serverAuth or
            // clientAuth, so an intermediate whose extension lists neither,
            // such as one restricted to anyExtendedKeyUsage or to an
            // unrelated purpose, could never verify a peer in either role.
            if !is_anchor
                && facts
                    .extended_key_usage
                    .is_some_and(|usage| !usage.server_auth && !usage.client_auth)
            {
                return Err(TrustError::at(
                    TrustErrorKind::IntermediateExtendedKeyUsage,
                    item,
                ));
            }
            // Subjects are unique within a profile, also across a CA
            // rotation: with two CAs of one name, which CRL applies would
            // depend on the order of the files.
            if authorities
                .iter()
                .any(|earlier: &Authority<'_>| earlier.facts.subject == facts.subject)
            {
                return Err(TrustError::at(TrustErrorKind::DuplicateSubject, item));
            }
            authorities.push(Authority {
                item,
                certificate,
                facts,
                is_anchor,
            });
        }

        for (index, authority) in authorities.iter().enumerate() {
            check_issuance(&authorities, index, algorithms)
                .map_err(|kind| TrustError::at(kind, authority.item))?;
        }

        let crls = load_crls(material, &authorities, algorithms, now)?;

        // An intermediate revoked by its issuer cannot be part of the
        // profile.
        for (index, authority) in authorities.iter().enumerate() {
            if authority.is_anchor {
                continue;
            }
            let issuer = issuer_of(&authorities, index)
                .map_err(|kind| TrustError::at(kind, authority.item))?;
            let revoked = crls
                .iter()
                .find(|crl| crl.issuer == issuer)
                .map(|crl| crl.list.find_serial(&authority.facts.serial))
                .transpose()
                .map_err(|_| TrustError::at(TrustErrorKind::MalformedCrl, authority.item))?
                .flatten()
                .is_some();
            if revoked {
                return Err(TrustError::at(
                    TrustErrorKind::RevokedIntermediate,
                    authority.item,
                ));
            }
        }

        let valid_from = authorities
            .iter()
            .map(|authority| authority.facts.not_before)
            .chain(crls.iter().map(|crl| crl.this_update))
            .max()
            .unwrap_or(i64::MAX);
        let expires_at = authorities
            .iter()
            .map(|authority| authority.facts.not_after)
            .chain(crls.iter().map(|crl| crl.next_update))
            .min()
            .unwrap_or(i64::MIN);

        let mut trust_anchors = Vec::with_capacity(material.anchors.len());
        let mut intermediate_facts = Vec::with_capacity(material.intermediates.len());
        for authority in authorities {
            if authority.is_anchor {
                let anchor = anchor_from_trusted_cert(authority.certificate).map_err(|_| {
                    TrustError::at(TrustErrorKind::MalformedCertificate, authority.item)
                })?;
                trust_anchors.push(anchor.to_owned());
            } else {
                intermediate_facts.push(authority.facts);
            }
        }

        Ok(Self {
            anchors: trust_anchors,
            intermediates: material.intermediates.clone(),
            intermediate_facts,
            crls: crls.into_iter().map(|crl| crl.list).collect(),
            valid_from,
            expires_at,
        })
    }

    /// The number of trust anchors.
    pub fn anchor_count(&self) -> usize {
        self.anchors.len()
    }

    /// The number of intermediate CA certificates.
    pub fn intermediate_count(&self) -> usize {
        self.intermediates.len()
    }

    /// The number of CRLs: one per CA certificate.
    pub fn crl_count(&self) -> usize {
        self.crls.len()
    }

    /// The first instant at which a CA certificate or CRL of this profile
    /// is no longer valid. No handshake verifies against the profile from
    /// then on, and streams it authenticated end no later.
    pub fn expires_at(&self) -> UnixTime {
        unix_time(self.expires_at)
    }

    /// Whether every certificate and CRL of the profile is valid at `now`.
    pub fn is_current(&self, now: UnixTime) -> bool {
        let now = unix_seconds(now);
        self.valid_from <= now && now < self.expires_at
    }
}

/// Read one configured CA certificate and hold it to the issuer profile.
fn read_authority(
    certificate: &CertificateDer<'static>,
    now: i64,
) -> Result<CertificateFacts, TrustErrorKind> {
    let facts = CertificateFacts::read(certificate).map_err(|error| match error {
        FactsError::TooLarge => TrustErrorKind::CertificateTooLarge,
        FactsError::Malformed => TrustErrorKind::MalformedCertificate,
        FactsError::TooManyExtensions => TrustErrorKind::TooManyExtensions,
        FactsError::TooManyNames => TrustErrorKind::TooManyNames,
    })?;
    // webpki's parse is the strict one: version 3, matching inner and outer
    // signature algorithms, no trailing data and no critical extension it
    // does not process. A trust anchor is held to it like any other CA.
    if let Err(error) = EndEntityCert::try_from(certificate) {
        return Err(match error {
            webpki::Error::UnsupportedCriticalExtension => {
                TrustErrorKind::UnsupportedCriticalExtension
            }
            webpki::Error::SignatureAlgorithmMismatch => TrustErrorKind::SignatureAlgorithmMismatch,
            _ => TrustErrorKind::MalformedCertificate,
        });
    }
    if !facts.is_ca {
        return Err(TrustErrorKind::NotCa);
    }
    let usage = facts.key_usage.ok_or(TrustErrorKind::MissingKeyUsage)?;
    if !usage.key_cert_sign {
        return Err(TrustErrorKind::CertSignUsage);
    }
    // Every configured CA has a CRL, so every one must be allowed to sign
    // it.
    if !usage.crl_sign {
        return Err(TrustErrorKind::CrlSignUsage);
    }
    if now < facts.not_before {
        return Err(TrustErrorKind::NotYetValid);
    }
    if now >= facts.not_after {
        return Err(TrustErrorKind::Expired);
    }
    if facts
        .name_constraints
        .is_some_and(|constraints| constraints.other_name)
    {
        return Err(TrustErrorKind::UnsupportedNameConstraint);
    }
    Ok(facts)
}

/// The position of the CA that issued `authorities[index]`.
fn issuer_of(authorities: &[Authority<'_>], index: usize) -> Result<usize, TrustErrorKind> {
    let issuer_name = &authorities[index].facts.issuer;
    authorities
        .iter()
        .position(|candidate| &candidate.facts.subject == issuer_name)
        .ok_or(TrustErrorKind::UnknownIssuer)
}

/// Check that `authorities[index]` was issued by a configured CA and that
/// its issuer chain reaches a trust anchor.
fn check_issuance(
    authorities: &[Authority<'_>],
    index: usize,
    algorithms: &WebPkiSupportedAlgorithms,
) -> Result<(), TrustErrorKind> {
    let authority = &authorities[index];
    if authority.is_anchor {
        // Trust comes from the operator's provisioning, not from the
        // self-signature; the signature shows the certificate is intact
        // and uses an algorithm the provider verifies.
        if authority.facts.issuer != authority.facts.subject {
            return Err(TrustErrorKind::AnchorNotSelfIssued);
        }
        return check_signature(authority.certificate, authority.certificate, algorithms);
    }

    let issuer = &authorities[issuer_of(authorities, index)?];
    if let (Some(authority_key), Some(subject_key)) = (
        &authority.facts.authority_key_identifier,
        &issuer.facts.subject_key_identifier,
    ) {
        if authority_key != subject_key {
            return Err(TrustErrorKind::KeyIdentifierMismatch);
        }
    }
    check_signature(issuer.certificate, authority.certificate, algorithms)?;

    // Walk to a trust anchor. `below` counts the CA certificates that
    // follow the current issuer on the way down to this one, which is what
    // a path length constraint limits (RFC 5280 §4.2.1.9).
    let mut current = index;
    let mut below: usize = 0;
    loop {
        let parent = issuer_of(authorities, current)?;
        below += 1;
        if parent == current || below > authorities.len() {
            return Err(TrustErrorKind::IssuerCycle);
        }
        let issuer = &authorities[parent];
        if issuer
            .facts
            .path_len
            .is_some_and(|limit| usize::try_from(limit).is_ok_and(|limit| below > limit))
        {
            return Err(TrustErrorKind::PathLength);
        }
        if issuer.is_anchor {
            return Ok(());
        }
        current = parent;
    }
}

fn check_signature(
    signer: &CertificateDer<'static>,
    signed: &CertificateDer<'static>,
    algorithms: &WebPkiSupportedAlgorithms,
) -> Result<(), TrustErrorKind> {
    verify_signed_by(signer, signed, algorithms).map_err(|failure| match failure {
        SignatureFailure::Malformed => TrustErrorKind::MalformedCertificate,
        SignatureFailure::UnsupportedAlgorithm => TrustErrorKind::UnsupportedSignatureAlgorithm,
        SignatureFailure::Invalid => TrustErrorKind::BadSignature,
    })
}

/// Validate every CRL and require exactly one per configured CA.
fn load_crls(
    material: &PrivateCaMaterial,
    authorities: &[Authority<'_>],
    algorithms: &WebPkiSupportedAlgorithms,
    now: i64,
) -> Result<Vec<LoadedCrl>, TrustError> {
    let issuers: Vec<CrlIssuer<'_>> = authorities
        .iter()
        .map(|authority| CrlIssuer {
            certificate: authority.certificate,
            subject: &authority.facts.subject,
            subject_key_identifier: authority.facts.subject_key_identifier.as_deref(),
        })
        .collect();

    let mut crls: Vec<LoadedCrl> = Vec::with_capacity(material.crls.len());
    for (index, der) in material.crls.iter().enumerate() {
        let item = TrustItem::Crl(index);
        let crl =
            load_crl(der, &issuers, algorithms, now).map_err(|kind| TrustError::at(kind, item))?;
        // Two CRLs of one issuer are refused even when identical: webpki
        // consults the first one it finds, so which of a clean and a
        // revoking CRL applies would depend on their order.
        if crls.iter().any(|earlier| earlier.issuer == crl.issuer) {
            return Err(TrustError::at(TrustErrorKind::DuplicateCrl, item));
        }
        crls.push(crl);
    }
    for (index, authority) in authorities.iter().enumerate() {
        if !crls.iter().any(|crl| crl.issuer == index) {
            return Err(TrustError::at(TrustErrorKind::MissingCrl, authority.item));
        }
    }
    Ok(crls)
}

/// Seconds since the Unix epoch, saturating far beyond any certificate.
pub(super) fn unix_seconds(time: UnixTime) -> i64 {
    i64::try_from(time.as_secs()).unwrap_or(i64::MAX)
}

/// The instant `seconds` after the Unix epoch; instants before it clamp to
/// the epoch.
pub(super) fn unix_time(seconds: i64) -> UnixTime {
    UnixTime::since_unix_epoch(std::time::Duration::from_secs(
        u64::try_from(seconds).unwrap_or(0),
    ))
}
