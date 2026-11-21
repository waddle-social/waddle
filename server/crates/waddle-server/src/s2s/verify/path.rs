//! The policy core shared by both TLS roles.
//!
//! A chain is accepted only when, in this order, it is within the size
//! ceilings, the trust profile is current, the end-entity certificate meets
//! the leaf profile, webpki verifies a signed path to one of the profile's
//! trust anchors (with full-chain revocation on the private-CA profile),
//! every issuer of that path meets the issuer profile, and the end-entity
//! certificate names the reference identity. A name that matches in a chain
//! that failed any earlier check authenticates nothing.

use std::cell::{Cell, RefCell};

use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, UnixTime};
use sha2::{Digest, Sha256};
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::s2s::TlsPeer;
use webpki::{
    CertRevocationList, EndEntityCert, ExpirationPolicy, ExtendedKeyUsageValidator, KeyPurposeId,
    KeyPurposeIdIter, KeyUsage, RequiredEkuNotFoundContext, RevocationCheckDepth,
    RevocationOptionsBuilder, UnknownStatusPolicy, VerifiedPath,
};

use super::der::name_constraint_forms;
use super::error::VerifyError;
use super::facts::{CertificateFacts, FactsError};
use super::identity::{match_reference, MatchedIdentifier, ReferenceIdentity};
use super::inventory::{unix_seconds, unix_time};
use super::limits::{max_public_leaf_validity_secs, MAX_CHAIN_CERTIFICATES, MAX_CHAIN_DER_BYTES};
use super::trust::{TrustProfile, TrustProfileKind};

/// id-kp-serverAuth, 1.3.6.1.5.5.7.3.1.
const SERVER_AUTH: KeyPurposeId<'static> =
    KeyPurposeId::new(&[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01]);
/// id-kp-clientAuth, 1.3.6.1.5.5.7.3.2.
const CLIENT_AUTH: KeyPurposeId<'static> =
    KeyPurposeId::new(&[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02]);

/// The TLS role in which a peer presented its certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CertificateRole {
    /// Presented by the initiating server, which is the TLS client. Its
    /// extended key usage must include serverAuth; on a private-CA profile
    /// clientAuth also suffices. Public CAs no longer issue clientAuth, so a
    /// serverAuth-only certificate is accepted here, which departs from
    /// RFC 5280 §4.2.1.12 (ADR-0020 D2). On the public-WebPKI profile a
    /// clientAuth-only certificate is refused: a publicly trusted CA attests
    /// a domain name only through a serverAuth certificate.
    TlsClient,
    /// Presented by the receiving server, which is the TLS server. Its
    /// extended key usage must include serverAuth.
    TlsServer,
}

impl CertificateRole {
    /// Whether an extended key usage of `client_auth` alone admits a
    /// certificate in this role under `profile`.
    fn accepts_client_auth(self, profile: TrustProfileKind) -> bool {
        self == Self::TlsClient && profile == TrustProfileKind::PrivateCa
    }
}

/// What a completed certificate verification established.
///
/// It proves that the peer holds a certificate for [`VerifiedPeer::domain`]
/// under that domain's trust profile once the TLS handshake that carried it
/// has completed, which [`super::PeerVerifier::established`] checks. It is
/// not an authorization: admission and SASL EXTERNAL are decided by the
/// connection owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPeer {
    domain: CanonicalDomain,
    matched: MatchedIdentifier,
    profile: TrustProfileKind,
    role: CertificateRole,
    certificate_sha256: [u8; 32],
    expires_at: i64,
}

impl VerifiedPeer {
    /// The reference identity the certificate proved: the stream's source
    /// domain, never a name taken from the certificate.
    pub fn domain(&self) -> &CanonicalDomain {
        &self.domain
    }

    /// Which presented identifier matched.
    pub fn matched_identifier(&self) -> MatchedIdentifier {
        self.matched
    }

    /// The trust profile the chain was verified under.
    pub fn profile(&self) -> TrustProfileKind {
        self.profile
    }

    /// The TLS role in which the certificate was presented.
    pub fn role(&self) -> CertificateRole {
        self.role
    }

    /// The SHA-256 digest of the end-entity certificate.
    pub fn certificate_sha256(&self) -> &[u8; 32] {
        &self.certificate_sha256
    }

    /// The first instant at which the evidence no longer holds: the
    /// earliest expiry among the end-entity certificate, the issuers of its
    /// path and, on the private-CA profile, every CA certificate and CRL of
    /// the profile. A stream authenticated by this certificate ends no
    /// later (RFC 6120 §13.7.2.3).
    pub fn expires_at(&self) -> UnixTime {
        unix_time(self.expires_at)
    }

    /// Whether the evidence still holds at `now`. The connection owner
    /// checks this again before it completes SASL EXTERNAL.
    pub fn is_current(&self, now: UnixTime) -> bool {
        unix_seconds(now) < self.expires_at
    }

    /// The negotiation input for a stream whose TLS handshake completed
    /// with this certificate.
    pub fn tls_peer(&self) -> TlsPeer {
        TlsPeer::Verified(self.domain.clone())
    }
}

/// The extended key usage webpki applies to the end-entity certificate
/// and every intermediate of a path: serverAuth, or on a private-CA profile
/// in the initiating role also clientAuth. anyExtendedKeyUsage alone
/// satisfies neither. A certificate without the extension passes here, as
/// RFC 5280 allows for CA certificates; the end-entity certificate must
/// carry it, which [`check_leaf`] enforces. webpki does not apply a trust
/// anchor's extended key usage; a private-CA profile checks its anchors when
/// it is loaded.
struct RoleUsage {
    role: CertificateRole,
    profile: TrustProfileKind,
}

impl ExtendedKeyUsageValidator for RoleUsage {
    fn validate(&self, purposes: KeyPurposeIdIter<'_, '_>) -> Result<(), webpki::Error> {
        let mut present = Vec::new();
        for purpose in purposes {
            let purpose = purpose?;
            if purpose == SERVER_AUTH
                || (purpose == CLIENT_AUTH && self.role.accepts_client_auth(self.profile))
            {
                return Ok(());
            }
            present.push(purpose.to_decoded_oid());
        }
        if present.is_empty() {
            return Ok(());
        }
        Err(webpki::Error::RequiredEkuNotFoundContext(
            RequiredEkuNotFoundContext {
                required: KeyUsage::server_auth(),
                present,
            },
        ))
    }
}

/// What the issuers of a verified path contribute to the outcome.
#[derive(Debug, Clone, Copy)]
struct PathFacts {
    /// Whether the trust anchor or an intermediate carries name
    /// constraints.
    name_constrained: bool,
    /// The earliest expiry among the path's intermediates.
    not_after: i64,
}

/// Verify a presented chain for `reference` under `profile`.
pub(super) fn verify_chain(
    reference: &ReferenceIdentity,
    profile: &TrustProfile,
    role: CertificateRole,
    end_entity: &CertificateDer<'_>,
    presented: &[CertificateDer<'_>],
    now: UnixTime,
    algorithms: &WebPkiSupportedAlgorithms,
) -> Result<VerifiedPeer, VerifyError> {
    if presented.len() >= MAX_CHAIN_CERTIFICATES {
        return Err(VerifyError::ChainTooLong);
    }
    let chain_bytes = presented
        .iter()
        .fold(end_entity.len(), |bytes, certificate| {
            bytes.saturating_add(certificate.len())
        });
    if chain_bytes > MAX_CHAIN_DER_BYTES {
        return Err(VerifyError::ChainTooLarge);
    }

    // Every presented certificate is read once, within the ceilings,
    // whether or not the path ends up using it.
    let leaf = read_facts(end_entity)?;
    let presented_facts = presented
        .iter()
        .map(|certificate| read_facts(certificate))
        .collect::<Result<Vec<_>, _>>()?;

    let now_seconds = unix_seconds(now);
    if let TrustProfile::PrivateCa(private) = profile {
        if now_seconds < private.valid_from {
            return Err(VerifyError::TrustNotYetValid);
        }
        if now_seconds >= private.expires_at {
            return Err(VerifyError::TrustExpired);
        }
    }

    check_leaf(&leaf, role, profile.kind())?;

    // The private-CA profile builds paths from its own validated
    // intermediates, so every issuer has a CRL; intermediates the peer sent
    // are never trusted. The public profile uses the peer's.
    let (anchors, intermediates, intermediate_facts) = match profile {
        TrustProfile::PublicWebPki(roots) => (&roots.anchors[..], presented, &presented_facts[..]),
        TrustProfile::PrivateCa(private) => (
            &private.anchors[..],
            &private.intermediates[..],
            &private.intermediate_facts[..],
        ),
    };
    let crls: Vec<&CertRevocationList<'_>> = match profile {
        TrustProfile::PublicWebPki(_) => Vec::new(),
        TrustProfile::PrivateCa(private) => private.crls.iter().collect(),
    };
    let revocation = match profile {
        TrustProfile::PublicWebPki(_) => None,
        TrustProfile::PrivateCa(_) => Some(
            RevocationOptionsBuilder::new(&crls)
                .map_err(|_| VerifyError::Path(webpki::Error::UnknownRevocationStatus))?
                .with_depth(RevocationCheckDepth::Chain)
                .with_status_policy(UnknownStatusPolicy::Deny)
                .with_expiration_policy(ExpirationPolicy::Enforce)
                .build(),
        ),
    };

    let certificate = EndEntityCert::try_from(end_entity).map_err(VerifyError::Path)?;
    // webpki offers each candidate path that verified up to a trust anchor.
    // A candidate that breaks the issuer profile is refused as an unknown
    // issuer, and webpki looks for another; the path it returns has
    // therefore passed. When no path passes, webpki reports the most
    // specific failure among all candidates.
    let policy_failure: RefCell<Option<VerifyError>> = RefCell::new(None);
    let selected: Cell<Option<PathFacts>> = Cell::new(None);
    let check_path = |path: &VerifiedPath<'_>| -> Result<(), webpki::Error> {
        match path_facts(path, intermediates, intermediate_facts) {
            Ok(facts) => {
                selected.set(Some(facts));
                Ok(())
            }
            Err(failure) => {
                policy_failure.replace(Some(failure));
                Err(webpki::Error::UnknownIssuer)
            }
        }
    };
    let path = match certificate.verify_for_usage(
        algorithms.all,
        anchors,
        intermediates,
        now,
        RoleUsage {
            role,
            profile: profile.kind(),
        },
        revocation,
        Some(&check_path),
    ) {
        Ok(_) => selected
            .get()
            .ok_or(VerifyError::Path(webpki::Error::UnknownIssuer))?,
        // An unknown issuer is also what a refused candidate reports, so the
        // issuer-profile failure behind it is the better explanation. Any
        // other error, such as a revoked certificate or an exhausted search
        // budget, outranked the refused candidates and is reported as is.
        Err(webpki::Error::UnknownIssuer) => {
            return Err(policy_failure
                .take()
                .unwrap_or(VerifyError::Path(webpki::Error::UnknownIssuer)));
        }
        Err(error) => return Err(VerifyError::Path(error)),
    };

    let matched = match_reference(
        reference,
        &certificate,
        &leaf.identifiers,
        profile.kind(),
        path.name_constrained,
    )?;

    let expires_at = match profile {
        TrustProfile::PublicWebPki(_) => leaf.not_after.min(path.not_after),
        TrustProfile::PrivateCa(private) => leaf.not_after.min(private.expires_at),
    };
    Ok(VerifiedPeer {
        domain: reference.domain().clone(),
        matched,
        profile: profile.kind(),
        role,
        certificate_sha256: Sha256::digest(end_entity.as_ref()).into(),
        expires_at,
    })
}

fn read_facts(certificate: &CertificateDer<'_>) -> Result<CertificateFacts, VerifyError> {
    CertificateFacts::read(certificate).map_err(|error| match error {
        FactsError::TooLarge => VerifyError::CertificateTooLarge,
        FactsError::Malformed => VerifyError::MalformedCertificate,
        FactsError::TooManyExtensions => VerifyError::TooManyExtensions,
        FactsError::TooManyNames => VerifyError::TooManyNames,
    })
}

/// The end-entity profile. webpki ignores key usage bits and accepts a
/// missing extended key usage; S2S does not.
fn check_leaf(
    leaf: &CertificateFacts,
    role: CertificateRole,
    profile: TrustProfileKind,
) -> Result<(), VerifyError> {
    if leaf.is_ca {
        return Err(VerifyError::LeafIsCa);
    }
    // Every TLS version and cipher suite of the profile authenticates the
    // peer with a signature. An absent keyUsage extension places no
    // restriction (RFC 5280 §4.2.1.3); the private-CA profile requires the
    // extension so that its issuance policy is explicit.
    match leaf.key_usage {
        Some(usage) if !usage.digital_signature => return Err(VerifyError::KeyUsage),
        None if profile == TrustProfileKind::PrivateCa => return Err(VerifyError::MissingKeyUsage),
        _ => {}
    }
    let usage = leaf
        .extended_key_usage
        .ok_or(VerifyError::MissingExtendedKeyUsage)?;
    if !usage.server_auth && !(usage.client_auth && role.accepts_client_auth(profile)) {
        return Err(VerifyError::ExtendedKeyUsage);
    }
    if profile == TrustProfileKind::PublicWebPki
        && leaf.not_after.saturating_sub(leaf.not_before)
            > max_public_leaf_validity_secs(leaf.not_before)
    {
        return Err(VerifyError::ValidityTooLong);
    }
    Ok(())
}

/// Hold the issuers of a candidate path to the issuer profile.
///
/// `candidates` are the certificates webpki built the path from and `facts`
/// their metadata, in the same order.
fn path_facts(
    path: &VerifiedPath<'_>,
    candidates: &[CertificateDer<'_>],
    facts: &[CertificateFacts],
) -> Result<PathFacts, VerifyError> {
    let mut result = PathFacts {
        name_constrained: false,
        not_after: i64::MAX,
    };
    if let Some(constraints) = path.anchor().name_constraints.as_ref() {
        let forms = name_constraint_forms(constraints.as_ref())
            .map_err(|_| VerifyError::UnsupportedNameConstraint)?;
        if forms.other_name {
            return Err(VerifyError::UnsupportedNameConstraint);
        }
        result.name_constrained = true;
    }
    for intermediate in path.intermediate_certificates() {
        let der = intermediate.der();
        let issuer = candidates
            .iter()
            .zip(facts)
            .find(|(candidate, _)| candidate.as_ref() == der.as_ref())
            .map(|(_, facts)| facts)
            .ok_or(VerifyError::IssuerUsage)?;
        // webpki requires the CA basic constraint but ignores key usage.
        if !issuer.is_ca || !issuer.key_usage.is_some_and(|usage| usage.key_cert_sign) {
            return Err(VerifyError::IssuerUsage);
        }
        if let Some(constraints) = issuer.name_constraints {
            // webpki enforces dNSName constraints and rejects an otherName
            // constraint only when the end-entity certificate has an
            // otherName entry. SRVName constraints (RFC 4985) are not
            // implemented, so a path that carries one is refused outright.
            if constraints.other_name {
                return Err(VerifyError::UnsupportedNameConstraint);
            }
            result.name_constrained = true;
        }
        result.not_after = result.not_after.min(issuer.not_after);
    }
    Ok(result)
}
