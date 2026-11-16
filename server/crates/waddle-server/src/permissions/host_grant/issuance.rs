//! Issuance rules for remote principals (ADR-0020 D8).
//!
//! A remote principal may hold a privileged grant or room moderation only
//! when its home domain is on the operator's trusted-home list. Issuance
//! therefore refuses an owner or admin affiliation for a JID whose domain is
//! neither served by this deployment nor a trusted home. Member-level and
//! negative affiliations are issued to any account.
//!
//! The trusted-home set is an input: this module neither loads nor
//! distributes it. A replica's policy takes it, with the served account
//! domains, from its [`PrincipalPolicy`] ([`IssuancePolicy::for_principals`]),
//! the value admission decides from too. Issuance records a privileged
//! grant for a trusted home; whether a stored grant is honoured is decided
//! where it is used, from the principal's witness at that decision
//! ([`super::PrincipalWitness`]): for a trusted home only on a `pkix`
//! stream, and only while the home stays admitted and listed.

use std::collections::BTreeSet;

use jid::BareJid;
use thiserror::Error;
use waddle_xmpp::identity::CanonicalDomain;

use super::model::{GrantSubject, RoomAffiliation};
use super::principal::PrincipalPolicy;

/// Why issuance accepted a grant, recorded with it as provenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssuanceBasis {
    /// The subject is an account of a domain this deployment serves.
    Local,
    /// The subject's home is a trusted home, so it may hold a privileged or
    /// moderator grant.
    TrustedHome,
    /// The subject is remote and the grant confers neither privilege nor
    /// moderation.
    Unprivileged,
}

impl IssuanceBasis {
    pub(crate) fn as_storage(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::TrustedHome => "trusted_home",
            Self::Unprivileged => "unprivileged",
        }
    }

    pub(crate) fn from_storage(value: &str) -> Option<Self> {
        [Self::Local, Self::TrustedHome, Self::Unprivileged]
            .into_iter()
            .find(|basis| basis.as_storage() == value)
    }
}

/// Where a grant subject's account is homed, as seen by the boundary that
/// decides whether to honour its grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubjectHome {
    /// An account of a domain this deployment serves.
    Served,
    /// An account of any other domain.
    Remote,
}

/// The account domains this deployment serves, in canonical form. An empty
/// set serves no account, so every subject is remote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServedAccountDomains(BTreeSet<String>);

impl ServedAccountDomains {
    pub fn new(domains: impl IntoIterator<Item = CanonicalDomain>) -> Self {
        Self(domain_texts(domains))
    }

    /// Where the account `subject` is homed. Domains match exactly.
    pub fn home_of(&self, subject: &BareJid) -> SubjectHome {
        if self.0.contains(subject.domain().as_str()) {
            SubjectHome::Served
        } else {
            SubjectHome::Remote
        }
    }

    fn contains(&self, domain: &str) -> bool {
        self.0.contains(domain)
    }
}

/// Why issuance refuses a grant.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum IssuanceRefusal {
    #[error(
        "a privileged or moderator grant to a remote account requires its home to be a trusted \
         home"
    )]
    UntrustedHome,
    #[error("a grant subject is an account, not an identity of a local service")]
    LocalService,
    #[error("a trusted home is a remote domain that can name a federation peer")]
    InvalidTrustedHome,
    #[error("a domain cannot be both served locally and a trusted home")]
    TrustedHomeIsLocal,
}

/// The inputs issuance decides from, and the [`PrincipalPolicy`] a
/// mutation's initiator is decided under.
///
/// Its account domains and trusted homes, and those the replica's
/// [`HostGrantAttachment`](super::HostGrantAttachment) decides admission
/// from, must come from one configuration value: a replica builds both from
/// its [`PrincipalPolicy`] ([`Self::for_principals`]). Issuance records an
/// owner or admin grant as `local` because its subject is an account of a
/// served domain, and admission honours the grant only if it agrees that
/// the domain is served; were the two sets configured apart, a grant issued
/// here could be honoured as no grant there, or the reverse.
#[derive(Clone, Debug, Default)]
pub struct IssuancePolicy {
    account_domains: ServedAccountDomains,
    service_domains: BTreeSet<String>,
    trusted_homes: BTreeSet<String>,
    principals: PrincipalPolicy,
}

impl IssuancePolicy {
    /// `account_domains` are the domains whose accounts this deployment
    /// homes, `service_domains` the domains of its own services (rooms and
    /// PubSub), and `trusted_homes` the operator's trusted-home list. A
    /// trusted home must be a fully qualified DNS name that this deployment
    /// does not serve; entries match exactly. A mutation's initiator is
    /// decided as on a replica with S2S off: no remote initiator's grant is
    /// honoured.
    pub fn new(
        account_domains: impl IntoIterator<Item = CanonicalDomain>,
        service_domains: impl IntoIterator<Item = CanonicalDomain>,
        trusted_homes: impl IntoIterator<Item = CanonicalDomain>,
    ) -> Result<Self, IssuanceRefusal> {
        let account_domains = ServedAccountDomains::new(account_domains);
        let service_domains = domain_texts(service_domains);
        let mut homes = BTreeSet::new();
        for home in trusted_homes {
            if !home.kind().names_federation_peer() {
                return Err(IssuanceRefusal::InvalidTrustedHome);
            }
            if account_domains.contains(home.as_str()) || service_domains.contains(home.as_str()) {
                return Err(IssuanceRefusal::TrustedHomeIsLocal);
            }
            homes.insert(home.as_str().to_owned());
        }
        Ok(Self {
            principals: PrincipalPolicy::local(account_domains.clone()),
            account_domains,
            service_domains,
            trusted_homes: homes,
        })
    }

    /// The policy of a replica deciding under `principals`: exactly its
    /// served account domains, the trusted-home list of the S2S snapshot it
    /// runs (none while S2S is off), and the domains of its own services. A
    /// mutation's initiator is decided under `principals` too.
    pub fn for_principals(
        principals: &PrincipalPolicy,
        service_domains: impl IntoIterator<Item = CanonicalDomain>,
    ) -> Result<Self, IssuanceRefusal> {
        let mut policy = Self::for_served(
            principals.served(),
            service_domains,
            principals.trusted_homes(),
        )?;
        policy.principals = principals.clone();
        Ok(policy)
    }

    /// A policy whose account domains are exactly `served`, as a replica's
    /// [`HostGrantAttachment`](super::HostGrantAttachment) holds them, so
    /// that issuance records an owner or admin grant as `local` for exactly
    /// the accounts admission honours it for. The other inputs are checked
    /// as [`Self::new`] checks them, and a mutation's initiator is decided
    /// as on a replica with S2S off.
    pub fn for_served(
        served: &ServedAccountDomains,
        service_domains: impl IntoIterator<Item = CanonicalDomain>,
        trusted_homes: impl IntoIterator<Item = CanonicalDomain>,
    ) -> Result<Self, IssuanceRefusal> {
        let mut policy = Self::new([], service_domains, [])?;
        policy.account_domains = served.clone();
        policy.principals = PrincipalPolicy::local(served.clone());
        for home in trusted_homes {
            if !home.kind().names_federation_peer() {
                return Err(IssuanceRefusal::InvalidTrustedHome);
            }
            if policy.account_domains.contains(home.as_str())
                || policy.service_domains.contains(home.as_str())
            {
                return Err(IssuanceRefusal::TrustedHomeIsLocal);
            }
            policy.trusted_homes.insert(home.as_str().to_owned());
        }
        Ok(policy)
    }

    /// Whether a privileged grant at any scope (deployment, community or
    /// room owner, admin or manager standing, or room moderation) may be
    /// written for `subject` under this policy: an account of a served
    /// domain, or of a trusted home. Bootstrap and repair writers ask this
    /// before they write such a grant to a legacy store (ADR-0020 D8).
    pub fn admits_privileged_subject(&self, subject: &jid::BareJid) -> bool {
        let domain = subject.domain().as_str();
        subject.node().is_some()
            && !self.service_domains.contains(domain)
            && (self.account_domains.contains(domain) || self.trusted_homes.contains(domain))
    }

    /// The account domains this deployment serves.
    pub fn served_account_domains(&self) -> &ServedAccountDomains {
        &self.account_domains
    }

    /// What a mutation's initiator is decided under.
    pub fn principals(&self) -> &PrincipalPolicy {
        &self.principals
    }

    /// Decide whether `affiliation` may be issued to `subject`.
    pub fn check(
        &self,
        subject: &GrantSubject,
        affiliation: RoomAffiliation,
    ) -> Result<IssuanceBasis, IssuanceRefusal> {
        let domain = subject.jid().domain().as_str();
        if self.service_domains.contains(domain) {
            return Err(IssuanceRefusal::LocalService);
        }
        if self.account_domains.contains(domain) {
            return Ok(IssuanceBasis::Local);
        }
        if !affiliation.confers_privilege_or_moderation() {
            return Ok(IssuanceBasis::Unprivileged);
        }
        if self.trusted_homes.contains(domain) {
            Ok(IssuanceBasis::TrustedHome)
        } else {
            Err(IssuanceRefusal::UntrustedHome)
        }
    }
}

fn domain_texts(domains: impl IntoIterator<Item = CanonicalDomain>) -> BTreeSet<String> {
    domains
        .into_iter()
        .map(|domain| domain.as_str().to_owned())
        .collect()
}
