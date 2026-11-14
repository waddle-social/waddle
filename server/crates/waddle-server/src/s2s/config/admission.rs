//! Peer admission under ADR-0020 D1, with the D2 dialback list and the D8
//! trusted-home list.
//!
//! Every entry and every queried domain is a [`CanonicalDomain`] in the
//! identity profile's canonical form. Allowlist, dialback and trusted-home
//! entries match exactly. A denylist entry covers its domain and every
//! subdomain, label by label, applies in every mode and overrides every
//! other list; unlike the other lists it may name domains outside `.test`
//! while admission is limited to `.test`. Only a fully qualified DNS name
//! can be admitted: an IP address, a numeric IPv4 form or a single-label
//! name never is.
//!
//! Admission grants transport only. It says nothing about communities,
//! rooms or any other protected effect, which host policy decides.

use std::collections::BTreeSet;

use waddle_xmpp::identity::CanonicalDomain;

use crate::s2s::verify::TrustProfileKind;

/// The admission mode of an enabled deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AdmissionMode {
    /// No remote domain is admitted.
    Closed,
    /// Only domains on the allowlist are admitted.
    #[default]
    Allowlist,
    /// Every domain that authenticates is admitted, unless denylisted.
    Open,
}

impl AdmissionMode {
    /// A stable code for logs.
    pub fn code(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Allowlist => "allowlist",
            Self::Open => "open",
        }
    }
}

/// Why a domain is not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdmissionRefusal {
    /// The domain is an IP address, a numeric IPv4 form or a single-label
    /// name (ADR-0020 D3).
    NotAPeerDomain,
    /// This release admits only domains within `.test` (ADR-0020 gate G6).
    OutsideTestDomain,
    /// The domain is one this deployment serves.
    LocalDomain,
    /// A denylist entry covers the domain.
    Denylisted,
    /// The deployment runs in [`AdmissionMode::Closed`].
    Closed,
    /// The deployment runs in [`AdmissionMode::Allowlist`] and the domain is
    /// not on the allowlist.
    NotAllowlisted,
}

impl AdmissionRefusal {
    /// A stable code for logs.
    pub fn code(self) -> &'static str {
        match self {
            Self::NotAPeerDomain => "not_a_peer_domain",
            Self::OutsideTestDomain => "outside_test_domain",
            Self::LocalDomain => "local_domain",
            Self::Denylisted => "denylisted",
            Self::Closed => "closed",
            Self::NotAllowlisted => "not_allowlisted",
        }
    }
}

/// The admission decision for one remote domain.
pub type AdmissionDecision = Result<(), AdmissionRefusal>;

/// The validated admission configuration of one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionPolicy {
    mode: AdmissionMode,
    local: BTreeSet<CanonicalDomain>,
    allow: BTreeSet<CanonicalDomain>,
    deny: BTreeSet<CanonicalDomain>,
    dialback: BTreeSet<CanonicalDomain>,
    trusted_homes: BTreeSet<CanonicalDomain>,
    private_ca_required: BTreeSet<CanonicalDomain>,
}

/// The `.test` top-level domain (RFC 6761 §6.2).
const TEST_DOMAIN: &str = "test";

impl AdmissionPolicy {
    /// Assemble a policy from lists the configuration loader has validated:
    /// every entry is a DNS name, no list repeats an entry, no list names a
    /// local domain and the denylist covers no entry of the other lists.
    pub(super) fn new(
        mode: AdmissionMode,
        local: BTreeSet<CanonicalDomain>,
        allow: BTreeSet<CanonicalDomain>,
        deny: BTreeSet<CanonicalDomain>,
        dialback: BTreeSet<CanonicalDomain>,
        trusted_homes: BTreeSet<CanonicalDomain>,
    ) -> Self {
        Self {
            mode,
            local,
            allow,
            deny,
            dialback,
            trusted_homes,
            private_ca_required: BTreeSet::new(),
        }
    }

    /// Require a private-CA profile of these trusted homes; the loader has
    /// checked that each is a trusted home with such a profile.
    pub(super) fn with_private_ca_required(mut self, domains: BTreeSet<CanonicalDomain>) -> Self {
        self.private_ca_required = domains;
        self
    }

    /// The mode.
    pub fn mode(&self) -> AdmissionMode {
        self.mode
    }

    /// Whether a stream with `domain` as its remote domain may be
    /// negotiated, checked at stream open and again before SASL EXTERNAL
    /// succeeds.
    pub fn decide(&self, domain: &CanonicalDomain) -> AdmissionDecision {
        if !domain.kind().names_federation_peer() {
            return Err(AdmissionRefusal::NotAPeerDomain);
        }
        if self.local.contains(domain) {
            return Err(AdmissionRefusal::LocalDomain);
        }
        // The denylist may already name domains outside `.test`, so its
        // refusal does not change when the restriction below is lifted.
        if self.is_denylisted(domain) {
            return Err(AdmissionRefusal::Denylisted);
        }
        // The discovery slice lifts this restriction through its own
        // reviewed change (ADR-0020 gate G6).
        if !is_test_domain(domain) {
            return Err(AdmissionRefusal::OutsideTestDomain);
        }
        match self.mode {
            AdmissionMode::Closed => Err(AdmissionRefusal::Closed),
            AdmissionMode::Allowlist if !self.allow.contains(domain) => {
                Err(AdmissionRefusal::NotAllowlisted)
            }
            AdmissionMode::Allowlist | AdmissionMode::Open => Ok(()),
        }
    }

    /// Whether a denylist entry covers `domain`: names it or one of its
    /// parent domains.
    pub fn is_denylisted(&self, domain: &CanonicalDomain) -> bool {
        self.deny.iter().any(|entry| domain.is_within(entry))
    }

    /// Whether `domain` may authenticate by inbound dialback: it is
    /// admitted and on the dialback list. Dialback itself is not
    /// implemented in this release, so nothing consults this yet.
    pub fn permits_dialback(&self, domain: &CanonicalDomain) -> bool {
        self.decide(domain).is_ok() && self.dialback.contains(domain)
    }

    /// Whether `domain` is a trusted home (ADR-0020 D8) for a stream whose
    /// certificate verified under `profile`: it is admitted, on the
    /// trusted-home list and, when the operator requires it for that home,
    /// authenticated under a private-CA profile. The list is configuration,
    /// not proof. A trusted home on the public-WebPKI profile is accepted
    /// with that profile's residual risk: public chains are not checked for
    /// revocation.
    pub fn is_trusted_home(&self, domain: &CanonicalDomain, profile: TrustProfileKind) -> bool {
        self.decide(domain).is_ok()
            && self.trusted_homes.contains(domain)
            && (profile == TrustProfileKind::PrivateCa
                || !self.private_ca_required.contains(domain))
    }

    /// Whether streams with `domain` may use the quarter of the stream and
    /// peer caps that `open` mode reserves for known peers: the domain is
    /// on the allowlist or the trusted-home list. In `allowlist` mode every
    /// admitted domain is on the allowlist, so the reserve is never held
    /// back from one. This says nothing about admission, which
    /// [`AdmissionPolicy::decide`] checks.
    pub fn is_reserved_peer(&self, domain: &CanonicalDomain) -> bool {
        self.mode != AdmissionMode::Open
            || self.allow.contains(domain)
            || self.trusted_homes.contains(domain)
    }

    /// Whether the stream and peer caps hold a reserve back from some
    /// admitted peers: only in `open` mode, where an admitted peer need be
    /// neither allowlisted nor a trusted home. In every other mode each
    /// admitted peer may use the whole of each cap, so an anonymous new
    /// connection may make room by evicting any stream.
    pub fn holds_reserve(&self) -> bool {
        self.mode == AdmissionMode::Open
    }

    /// Whether the operator requires `domain`, a trusted home, to
    /// authenticate under a private-CA profile.
    pub fn requires_private_ca(&self, domain: &CanonicalDomain) -> bool {
        self.private_ca_required.contains(domain)
    }

    /// The allowlist.
    pub fn allowlist(&self) -> &BTreeSet<CanonicalDomain> {
        &self.allow
    }

    /// The denylist.
    pub fn denylist(&self) -> &BTreeSet<CanonicalDomain> {
        &self.deny
    }

    /// The inbound dialback list.
    pub fn dialback_list(&self) -> &BTreeSet<CanonicalDomain> {
        &self.dialback
    }

    /// The trusted-home list.
    pub fn trusted_homes(&self) -> &BTreeSet<CanonicalDomain> {
        &self.trusted_homes
    }
}

/// Whether `domain` is within the `.test` top-level domain.
pub(super) fn is_test_domain(domain: &CanonicalDomain) -> bool {
    CanonicalDomain::parse_canonical(TEST_DOMAIN).is_ok_and(|test| domain.is_within(&test))
}
