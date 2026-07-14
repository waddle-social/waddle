use std::collections::{HashMap, HashSet};

use jid::BareJid;
use serde::{Deserialize, Serialize};

use crate::types::Affiliation;

/// Policy for controlling which federated users can join a MUC room.
///
/// This enum defines how a room handles join requests from users on remote
/// XMPP servers (i.e., users with JIDs from different domains).
///
/// ## Policy Types
///
/// - **Open**: Any federated user can join (subject to other room restrictions)
/// - **AllowList**: Only users from explicitly allowed domains/JIDs can join
/// - **BlockList**: Block specific domains/JIDs; all others allowed
/// - **Closed**: No federation - only local users can join
///
/// ## Example
///
/// ```ignore
/// use waddle_xmpp::muc::affiliation::FederatedPermissionPolicy;
///
/// // Allow anyone from any federated server
/// let open = FederatedPermissionPolicy::Open;
///
/// // Only allow users from trusted.example.com
/// let allowlist = FederatedPermissionPolicy::AllowList;
///
/// // Block spam.example.com but allow everyone else
/// let blocklist = FederatedPermissionPolicy::BlockList;
///
/// // No federation at all
/// let closed = FederatedPermissionPolicy::Closed;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum FederatedPermissionPolicy {
    /// Any federated user can join (default for open rooms).
    ///
    /// This is the most permissive policy - any user from any remote
    /// server can join the room, subject to other room restrictions
    /// (e.g., members-only, password-protected).
    #[default]
    Open,

    /// Only users from explicitly allowed domains or with allowed JIDs can join.
    ///
    /// Use this for rooms that should only federate with specific trusted
    /// servers. The allow list is configured in `FederatedAffiliationConfig`.
    AllowList,

    /// Block specific domains or JIDs; all others are allowed.
    ///
    /// Use this to block known spam or abusive servers while still
    /// allowing general federation. The block list is configured in
    /// `FederatedAffiliationConfig`.
    BlockList,

    /// No federation - only local users can join.
    ///
    /// Use this for private rooms that should not be accessible to
    /// users from other servers at all.
    Closed,
}

impl FederatedPermissionPolicy {
    /// Returns true if this policy allows any federation at all.
    pub fn allows_federation(&self) -> bool {
        !matches!(self, FederatedPermissionPolicy::Closed)
    }

    /// Returns true if this is an open policy (no domain restrictions).
    pub fn is_open(&self) -> bool {
        matches!(self, FederatedPermissionPolicy::Open)
    }

    /// Returns true if this policy uses an allow list.
    pub fn uses_allow_list(&self) -> bool {
        matches!(self, FederatedPermissionPolicy::AllowList)
    }

    /// Returns true if this policy uses a block list.
    pub fn uses_block_list(&self) -> bool {
        matches!(self, FederatedPermissionPolicy::BlockList)
    }
}

/// Configuration for federated user affiliations.
///
/// This struct stores the default affiliation assigned to federated users
/// and domain-specific overrides. It works in conjunction with
/// `FederatedPermissionPolicy` to control federation access.
///
/// ## Example
///
/// ```ignore
/// use waddle_xmpp::muc::affiliation::{FederatedAffiliationConfig, FederatedPermissionPolicy};
/// use waddle_xmpp::types::Affiliation;
///
/// let mut config = FederatedAffiliationConfig::new(Affiliation::Member);
///
/// // Allow specific domains
/// config.add_allowed_domain("trusted.example.com");
/// config.add_allowed_domain("partner.example.org");
///
/// // Block a spammy domain
/// config.add_blocked_domain("spam.example.net");
///
/// // Give users from a partner domain higher affiliation
/// config.set_domain_affiliation("partner.example.org", Affiliation::Admin);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederatedAffiliationConfig {
    /// Default affiliation for federated users (typically Member or None).
    pub default_affiliation: Affiliation,

    /// Domains explicitly allowed (used with AllowList policy).
    pub allowed_domains: HashSet<String>,

    /// Domains explicitly blocked (used with BlockList policy).
    pub blocked_domains: HashSet<String>,

    /// JIDs explicitly allowed (used with AllowList policy).
    /// These take precedence over domain-level rules.
    pub allowed_jids: HashSet<BareJid>,

    /// JIDs explicitly blocked (used with BlockList policy).
    /// These take precedence over domain-level rules.
    pub blocked_jids: HashSet<BareJid>,

    /// Domain-specific affiliation overrides.
    /// Allows giving different default affiliations to users from specific domains.
    pub domain_affiliations: HashMap<String, Affiliation>,

    /// JID-specific affiliation overrides.
    /// Allows giving specific affiliations to individual federated users.
    pub jid_affiliations: HashMap<BareJid, Affiliation>,
}

impl FederatedAffiliationConfig {
    /// Create a new configuration with the specified default affiliation.
    pub fn new(default_affiliation: Affiliation) -> Self {
        Self {
            default_affiliation,
            allowed_domains: HashSet::new(),
            blocked_domains: HashSet::new(),
            allowed_jids: HashSet::new(),
            blocked_jids: HashSet::new(),
            domain_affiliations: HashMap::new(),
            jid_affiliations: HashMap::new(),
        }
    }

    /// Create a configuration that allows any federated user with Member affiliation.
    pub fn open_member() -> Self {
        Self::new(Affiliation::Member)
    }

    /// Create a configuration that allows any federated user with no affiliation.
    pub fn open_none() -> Self {
        Self::new(Affiliation::None)
    }

    /// Add a domain to the allow list.
    pub fn add_allowed_domain(&mut self, domain: impl Into<String>) {
        self.allowed_domains.insert(domain.into());
    }

    /// Remove a domain from the allow list.
    pub fn remove_allowed_domain(&mut self, domain: &str) -> bool {
        self.allowed_domains.remove(domain)
    }

    /// Add a domain to the block list.
    pub fn add_blocked_domain(&mut self, domain: impl Into<String>) {
        self.blocked_domains.insert(domain.into());
    }

    /// Remove a domain from the block list.
    pub fn remove_blocked_domain(&mut self, domain: &str) -> bool {
        self.blocked_domains.remove(domain)
    }

    /// Add a JID to the allow list.
    pub fn add_allowed_jid(&mut self, jid: BareJid) {
        self.allowed_jids.insert(jid);
    }

    /// Remove a JID from the allow list.
    pub fn remove_allowed_jid(&mut self, jid: &BareJid) -> bool {
        self.allowed_jids.remove(jid)
    }

    /// Add a JID to the block list.
    pub fn add_blocked_jid(&mut self, jid: BareJid) {
        self.blocked_jids.insert(jid);
    }

    /// Remove a JID from the block list.
    pub fn remove_blocked_jid(&mut self, jid: &BareJid) -> bool {
        self.blocked_jids.remove(jid)
    }

    /// Set a domain-specific affiliation override.
    pub fn set_domain_affiliation(&mut self, domain: impl Into<String>, affiliation: Affiliation) {
        self.domain_affiliations.insert(domain.into(), affiliation);
    }

    /// Remove a domain-specific affiliation override.
    pub fn remove_domain_affiliation(&mut self, domain: &str) -> Option<Affiliation> {
        self.domain_affiliations.remove(domain)
    }

    /// Set a JID-specific affiliation override.
    pub fn set_jid_affiliation(&mut self, jid: BareJid, affiliation: Affiliation) {
        self.jid_affiliations.insert(jid, affiliation);
    }

    /// Remove a JID-specific affiliation override.
    pub fn remove_jid_affiliation(&mut self, jid: &BareJid) -> Option<Affiliation> {
        self.jid_affiliations.remove(jid)
    }

    /// Check if a domain is in the allow list.
    pub fn is_domain_allowed(&self, domain: &str) -> bool {
        self.allowed_domains.contains(domain)
    }

    /// Check if a domain is in the block list.
    pub fn is_domain_blocked(&self, domain: &str) -> bool {
        self.blocked_domains.contains(domain)
    }

    /// Check if a JID is explicitly allowed.
    pub fn is_jid_allowed(&self, jid: &BareJid) -> bool {
        self.allowed_jids.contains(jid)
    }

    /// Check if a JID is explicitly blocked.
    pub fn is_jid_blocked(&self, jid: &BareJid) -> bool {
        self.blocked_jids.contains(jid)
    }

    /// Get the affiliation for a federated JID.
    ///
    /// Checks in order:
    /// 1. JID-specific affiliation override
    /// 2. Domain-specific affiliation override
    /// 3. Default affiliation
    pub fn get_affiliation_for_jid(&self, jid: &BareJid) -> Affiliation {
        if let Some(&affiliation) = self.jid_affiliations.get(jid) {
            return affiliation;
        }

        let domain = jid.domain().as_str();
        if let Some(&affiliation) = self.domain_affiliations.get(domain) {
            return affiliation;
        }

        self.default_affiliation
    }

    /// Check if a federated user is allowed to join based on the policy.
    ///
    /// This method evaluates whether a JID is permitted under the given policy:
    /// - **Open**: Always returns `true`
    /// - **AllowList**: Returns `true` only if JID or domain is in allow list
    /// - **BlockList**: Returns `true` unless JID or domain is in block list
    /// - **Closed**: Always returns `false`
    ///
    /// JID-level rules take precedence over domain-level rules.
    pub fn is_allowed_by_policy(&self, jid: &BareJid, policy: FederatedPermissionPolicy) -> bool {
        let domain = jid.domain().as_str();

        match policy {
            FederatedPermissionPolicy::Open => true,
            FederatedPermissionPolicy::AllowList => {
                self.is_jid_allowed(jid) || self.is_domain_allowed(domain)
            }
            FederatedPermissionPolicy::BlockList => {
                if self.is_jid_blocked(jid) {
                    return false;
                }
                if self.is_jid_allowed(jid) {
                    return true;
                }
                !self.is_domain_blocked(domain)
            }
            FederatedPermissionPolicy::Closed => false,
        }
    }
}
