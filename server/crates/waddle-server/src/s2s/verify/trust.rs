//! Trust profiles and the startup trust inventory (ADR-0020 D2).
//!
//! Every remote domain has exactly one trust profile, chosen from
//! configuration before a handshake starts: the private-CA profile the
//! operator configured for that domain, or otherwise the public-WebPKI
//! profile. A chain is verified against that profile only. A public
//! certificate therefore fails for a private-CA domain, a private
//! certificate fails for every other domain, and no second profile is tried
//! after a rejection.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, TrustAnchor, UnixTime};
use waddle_xmpp::identity::CanonicalDomain;
use webpki::anchor_from_trusted_cert;

use super::error::{TrustError, TrustErrorKind, TrustItem};
use super::inventory::PrivateCaProfile;
use super::limits::MAX_PUBLIC_ROOTS;

/// The trust anchors of the public-WebPKI profile.
pub struct PublicRoots {
    pub(super) anchors: Vec<TrustAnchor<'static>>,
}

impl fmt::Debug for PublicRoots {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicRoots")
            .field("anchors", &self.anchors.len())
            .finish()
    }
}

impl PublicRoots {
    /// The Mozilla CA program's TLS server roots as compiled into the
    /// linked `webpki-roots` release. The set changes only with a new
    /// build: nothing is read from the host or fetched at run time.
    pub fn mozilla() -> Self {
        Self {
            anchors: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        }
    }

    /// A substituted root set, for deployments and tests that must not
    /// trust the public roots. The certificates are taken as trust anchors
    /// as they are.
    pub fn from_certificates(certificates: &[CertificateDer<'_>]) -> Result<Self, TrustError> {
        if certificates.is_empty() {
            return Err(TrustError::new(TrustErrorKind::NoTrustAnchors));
        }
        if certificates.len() > MAX_PUBLIC_ROOTS {
            return Err(TrustError::new(TrustErrorKind::TooManyPublicRoots));
        }
        let anchors = certificates
            .iter()
            .enumerate()
            .map(|(index, certificate)| {
                anchor_from_trusted_cert(certificate)
                    .map(|anchor| anchor.to_owned())
                    .map_err(|_| {
                        TrustError::at(
                            TrustErrorKind::MalformedCertificate,
                            TrustItem::Anchor(index),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { anchors })
    }

    /// The number of trust anchors.
    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    /// Whether the set holds no trust anchor.
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }
}

/// Which kind of trust profile a domain uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustProfileKind {
    /// Public CAs, without revocation checking.
    PublicWebPki,
    /// The operator's own CAs, with complete CRLs.
    PrivateCa,
}

/// The trust profile selected for one remote domain.
#[derive(Debug, Clone)]
pub enum TrustProfile {
    PublicWebPki(Arc<PublicRoots>),
    PrivateCa(Arc<PrivateCaProfile>),
}

impl TrustProfile {
    /// The kind of this profile.
    pub fn kind(&self) -> TrustProfileKind {
        match self {
            Self::PublicWebPki(_) => TrustProfileKind::PublicWebPki,
            Self::PrivateCa(_) => TrustProfileKind::PrivateCa,
        }
    }
}

/// Counts describing a loaded [`TrustInventory`], for the startup log.
/// They contain no domain or certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustInventorySummary {
    pub public_roots: usize,
    /// Domains with a private-CA profile.
    pub private_domains: usize,
    /// Distinct private-CA profiles.
    pub private_profiles: usize,
    pub private_ca_certificates: usize,
    pub private_crls: usize,
    /// The first instant at which a private-CA profile expires.
    pub earliest_private_expiry: Option<UnixTime>,
    /// Peer domains that use the public-WebPKI profile although a domain
    /// they are within uses a private-CA profile, such as
    /// `muc.example.org` when only `example.org` is configured. A profile
    /// covers exactly the domain it is configured for, so such a domain is
    /// verified against the public roots; a non-zero count usually means a
    /// component domain was left out of the configuration.
    pub public_subdomains_of_private: usize,
}

impl TrustInventorySummary {
    /// Write the summary to the startup log. Only counts are logged; a
    /// non-zero [`Self::public_subdomains_of_private`] is logged as a
    /// warning.
    pub fn log(&self) {
        tracing::info!(
            public_roots = self.public_roots,
            private_domains = self.private_domains,
            private_profiles = self.private_profiles,
            private_ca_certificates = self.private_ca_certificates,
            private_crls = self.private_crls,
            earliest_private_expiry_unix = ?self.earliest_private_expiry.map(|expiry| expiry.as_secs()),
            "s2s trust inventory loaded"
        );
        if self.public_subdomains_of_private > 0 {
            tracing::warn!(
                count = self.public_subdomains_of_private,
                "s2s peer domains use the public-WebPKI profile although an enclosing domain uses a private-CA profile"
            );
        }
    }
}

/// All trust material of a process, loaded and validated before S2S starts.
#[derive(Debug)]
pub struct TrustInventory {
    public: Arc<PublicRoots>,
    private: BTreeMap<CanonicalDomain, Arc<PrivateCaProfile>>,
}

impl TrustInventory {
    /// An inventory in which every domain uses the public-WebPKI profile
    /// with `public` as its roots.
    pub fn new(public: PublicRoots) -> Self {
        Self {
            public: Arc::new(public),
            private: BTreeMap::new(),
        }
    }

    /// Give `domain` the private-CA `profile`. The match is exact: a
    /// subdomain of `domain` keeps the public-WebPKI profile unless it is
    /// configured itself, and [`TrustInventory::summary`] counts the peer
    /// domains left that way. Several domains may share one profile.
    pub fn configure_private_ca(
        &mut self,
        domain: CanonicalDomain,
        profile: Arc<PrivateCaProfile>,
    ) -> Result<(), TrustError> {
        if !domain.kind().names_federation_peer() {
            return Err(TrustError::new(TrustErrorKind::NotAPeerDomain));
        }
        if self.private.contains_key(&domain) {
            return Err(TrustError::new(TrustErrorKind::DuplicateDomain));
        }
        self.private.insert(domain, profile);
        Ok(())
    }

    /// The one trust profile of `domain`.
    pub fn profile_for(&self, domain: &CanonicalDomain) -> TrustProfile {
        match self.private.get(domain) {
            Some(profile) => TrustProfile::PrivateCa(Arc::clone(profile)),
            None => TrustProfile::PublicWebPki(Arc::clone(&self.public)),
        }
    }

    /// Counts for the startup log. `peer_domains` are the remote domains the
    /// deployment expects to federate with, such as the admission
    /// allowlist; they are used only to count those that fall under a
    /// private-CA domain without being on a private-CA profile themselves.
    pub fn summary<'a>(
        &self,
        peer_domains: impl IntoIterator<Item = &'a CanonicalDomain>,
    ) -> TrustInventorySummary {
        let public_subdomains_of_private = peer_domains
            .into_iter()
            .filter(|domain| {
                !self.private.contains_key(*domain)
                    && self.private.keys().any(|private| domain.is_within(private))
            })
            .collect::<BTreeSet<_>>()
            .len();
        let mut profiles: Vec<&Arc<PrivateCaProfile>> = Vec::new();
        for profile in self.private.values() {
            if !profiles.iter().any(|known| Arc::ptr_eq(known, profile)) {
                profiles.push(profile);
            }
        }
        TrustInventorySummary {
            public_roots: self.public.len(),
            private_domains: self.private.len(),
            private_profiles: profiles.len(),
            private_ca_certificates: profiles
                .iter()
                .map(|profile| profile.anchor_count() + profile.intermediate_count())
                .sum(),
            private_crls: profiles.iter().map(|profile| profile.crl_count()).sum(),
            earliest_private_expiry: profiles.iter().map(|profile| profile.expires_at()).min(),
            public_subdomains_of_private,
        }
    }
}
