//! Issuance rules for remote principals (ADR-0020 D8).
//!
//! A remote principal may hold a privileged grant or room moderation only
//! when its home domain is on the operator's trusted-home list. Issuance
//! therefore refuses an owner or admin affiliation for a JID whose domain is
//! neither served by this deployment nor a trusted home. Member-level and
//! negative affiliations are issued to any account.
//!
//! The trusted-home set is an input: this module neither loads nor
//! distributes it. Issuance records a privileged grant for a trusted home;
//! whether a stored grant is honoured is decided where it is used. Admission
//! and initiator authority honour a privileged or moderator grant only for
//! an account of a served domain ([`ServedAccountDomains`]), because
//! honouring one for a trusted home also depends on the authentication
//! strength of the stream the principal arrived on.

use std::collections::BTreeSet;

use jid::BareJid;
use thiserror::Error;
use waddle_xmpp::identity::CanonicalDomain;

use super::model::{GrantSubject, RoomAffiliation};

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

/// The inputs issuance decides from.
///
/// Its account domains and the served account domains of the replica's
/// [`HostGrantAttachment`](super::HostGrantAttachment), which admission
/// honours privileged grants for, must come from one configuration value.
/// Issuance records an owner or admin grant as `local` because its subject
/// is an account of a served domain, and admission honours the grant only
/// if it agrees that the domain is served; were the two sets configured
/// apart, a grant issued here could be honoured as no grant there, or the
/// reverse. Nothing in this slice builds a policy from configuration yet;
/// the one that does takes the domains from where the attachment takes
/// them.
#[derive(Clone, Debug, Default)]
pub struct IssuancePolicy {
    account_domains: ServedAccountDomains,
    service_domains: BTreeSet<String>,
    trusted_homes: BTreeSet<String>,
}

impl IssuancePolicy {
    /// `account_domains` are the domains whose accounts this deployment
    /// homes, `service_domains` the domains of its own services (rooms and
    /// PubSub), and `trusted_homes` the operator's trusted-home list. A
    /// trusted home must be a fully qualified DNS name that this deployment
    /// does not serve; entries match exactly.
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
            account_domains,
            service_domains,
            trusted_homes: homes,
        })
    }

    /// The account domains this deployment serves.
    pub fn served_account_domains(&self) -> &ServedAccountDomains {
        &self.account_domains
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
