//! MUC Affiliation Sync with Zanzibar Permissions
//!
//! This module implements the synchronization between Waddle's Zanzibar-based
//! permission system and XMPP MUC (Multi-User Chat) affiliations.
//!
//! ## Permission to Affiliation Mapping
//!
//! Per RFC-0002, Waddle permissions map to MUC affiliations as follows:
//! - `owner` -> Owner (highest privilege, can configure room)
//! - `admin` -> Admin (can manage members, kick users)
//! - `moderator` -> Admin (same as admin for MUC purposes)
//! - `member` -> Member (can join members-only rooms)
//! - `viewer` -> Member (read-only access maps to Member)
//! - No permission -> None (blocked from members-only rooms)
//!
//! ## Example
//!
//! ```ignore
//! use waddle_xmpp::muc::affiliation::{AffiliationResolver, PermissionMapper};
//!
//! // Check affiliation for a user joining a room
//! let affiliation = resolver.resolve_affiliation(&user_did, &channel_id).await?;
//! ```

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;

use jid::BareJid;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument, warn};

use crate::types::Affiliation;
use crate::XmppError;

// =============================================================================
// Federated Permission Policy
// =============================================================================

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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
        // Check JID-specific override first
        if let Some(&affiliation) = self.jid_affiliations.get(jid) {
            return affiliation;
        }

        // Check domain-specific override
        let domain = jid.domain().as_str();
        if let Some(&affiliation) = self.domain_affiliations.get(domain) {
            return affiliation;
        }

        // Fall back to default
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
                // JID must be explicitly allowed, or domain must be allowed
                self.is_jid_allowed(jid) || self.is_domain_allowed(domain)
            }

            FederatedPermissionPolicy::BlockList => {
                // JID must not be blocked, and domain must not be blocked
                // JID-specific rules take precedence
                if self.is_jid_blocked(jid) {
                    return false;
                }
                if self.is_jid_allowed(jid) {
                    // Explicitly allowed JIDs bypass domain blocks
                    return true;
                }
                !self.is_domain_blocked(domain)
            }

            FederatedPermissionPolicy::Closed => false,
        }
    }
}

/// Maps Waddle permission relations to MUC affiliations.
///
/// The mapping follows RFC-0002 specification for permission hierarchy.
#[derive(Debug, Clone)]
pub struct PermissionMapper {
    /// Custom mapping overrides (relation -> affiliation)
    custom_mappings: HashMap<String, Affiliation>,
}

impl Default for PermissionMapper {
    fn default() -> Self {
        Self::new()
    }
}

impl PermissionMapper {
    /// Create a new permission mapper with default mappings.
    pub fn new() -> Self {
        Self {
            custom_mappings: HashMap::new(),
        }
    }

    /// Add a custom mapping override.
    #[allow(dead_code)]
    pub fn with_mapping(mut self, relation: &str, affiliation: Affiliation) -> Self {
        self.custom_mappings
            .insert(relation.to_string(), affiliation);
        self
    }

    /// Map a Waddle permission relation to MUC affiliation.
    ///
    /// Returns the highest affiliation if multiple relations are present.
    pub fn map_relation(&self, relation: &str) -> Affiliation {
        // Check custom mappings first
        if let Some(affiliation) = self.custom_mappings.get(relation) {
            return *affiliation;
        }

        // Default mappings per RFC-0002
        match relation {
            "owner" => Affiliation::Owner,
            "admin" => Affiliation::Admin,
            "moderator" => Affiliation::Admin, // Moderator maps to Admin in MUC
            "manager" => Affiliation::Admin,   // Channel manager maps to Admin
            "member" => Affiliation::Member,
            "writer" => Affiliation::Member, // Writers are members
            "viewer" => Affiliation::Member, // Viewers are members (read-only)
            _ => Affiliation::None,
        }
    }

    /// Map multiple relations to the highest affiliation.
    ///
    /// When a user has multiple relations (e.g., both member and admin),
    /// the highest privilege wins.
    pub fn map_relations(&self, relations: &[String]) -> Affiliation {
        relations
            .iter()
            .map(|r| self.map_relation(r))
            .max()
            .unwrap_or(Affiliation::None)
    }
}

/// Trait for resolving MUC affiliations from external permission systems.
///
/// Implement this trait to connect the XMPP server to different
/// permission backends (Zanzibar, RBAC, etc.).
pub trait AffiliationResolver: Send + Sync {
    /// Resolve the affiliation for a user in a channel.
    ///
    /// # Arguments
    /// * `user_did` - The user's decentralized identifier (e.g., did:plc:...)
    /// * `waddle_id` - The Waddle community ID
    /// * `channel_id` - The channel ID within the Waddle
    ///
    /// # Returns
    /// The MUC affiliation for the user, or an error if resolution fails.
    fn resolve_affiliation(
        &self,
        user_did: &str,
        waddle_id: &str,
        channel_id: &str,
    ) -> impl Future<Output = Result<Affiliation, XmppError>> + Send;

    /// Get all users with a specific affiliation in a channel.
    ///
    /// Used for XEP-0045 affiliation list queries.
    fn list_affiliations(
        &self,
        waddle_id: &str,
        channel_id: &str,
        affiliation: Affiliation,
    ) -> impl Future<Output = Result<Vec<AffiliationEntry>, XmppError>> + Send;

    /// Check if a user can join a room.
    ///
    /// For members-only rooms, only users with Member+ affiliation can join.
    fn can_join(
        &self,
        user_did: &str,
        waddle_id: &str,
        channel_id: &str,
        members_only: bool,
    ) -> impl Future<Output = Result<bool, XmppError>> + Send;

    /// Resolve the affiliation for a federated (remote) user.
    ///
    /// This method determines what affiliation a user from a remote XMPP server
    /// should have when joining a room. It considers:
    /// 1. The room's federation policy (Open, AllowList, BlockList, Closed)
    /// 2. Domain and JID-specific overrides in the federated config
    /// 3. The default affiliation for federated users
    ///
    /// # Arguments
    /// * `jid` - The remote user's bare JID
    /// * `policy` - The room's federation permission policy
    /// * `config` - The room's federated affiliation configuration
    ///
    /// # Returns
    /// The affiliation for the federated user, or `None` if they're not allowed.
    fn resolve_federated_affiliation(
        &self,
        jid: &BareJid,
        policy: FederatedPermissionPolicy,
        config: &FederatedAffiliationConfig,
    ) -> Affiliation {
        // Check if the user is allowed by the policy
        if !config.is_allowed_by_policy(jid, policy) {
            return Affiliation::None;
        }

        // Get the affiliation from the config (checks JID, domain, then default)
        config.get_affiliation_for_jid(jid)
    }

    /// Check if a federated user can join a room.
    ///
    /// This combines the federation policy check with the affiliation check
    /// to determine if a remote user should be allowed to join.
    ///
    /// # Arguments
    /// * `jid` - The remote user's bare JID
    /// * `policy` - The room's federation permission policy
    /// * `config` - The room's federated affiliation configuration
    /// * `members_only` - Whether the room requires membership
    ///
    /// # Returns
    /// `true` if the user can join, `false` otherwise.
    fn can_federated_user_join(
        &self,
        jid: &BareJid,
        policy: FederatedPermissionPolicy,
        config: &FederatedAffiliationConfig,
        members_only: bool,
    ) -> bool {
        // First check if federation allows this user at all
        if !config.is_allowed_by_policy(jid, policy) {
            return false;
        }

        // Get their affiliation
        let affiliation = config.get_affiliation_for_jid(jid);

        // For members-only rooms, they need at least Member affiliation
        if members_only {
            affiliation >= Affiliation::Member
        } else {
            // For open rooms, they just need to not be banned
            affiliation != Affiliation::Outcast
        }
    }
}

/// An entry in the affiliation list.
#[derive(Debug, Clone)]
pub struct AffiliationEntry {
    /// The user's JID (bare)
    pub jid: BareJid,
    /// The user's affiliation
    pub affiliation: Affiliation,
    /// Optional reason/notes
    pub reason: Option<String>,
}

impl AffiliationEntry {
    /// Create a new affiliation entry.
    pub fn new(jid: BareJid, affiliation: Affiliation) -> Self {
        Self {
            jid,
            affiliation,
            reason: None,
        }
    }

    /// Create an affiliation entry with a reason.
    #[allow(dead_code)]
    pub fn with_reason(jid: BareJid, affiliation: Affiliation, reason: impl Into<String>) -> Self {
        Self {
            jid,
            affiliation,
            reason: Some(reason.into()),
        }
    }
}

/// Affiliation resolver that uses the AppState's check_permission method.
///
/// This resolver queries the Zanzibar permission system through the
/// AppState trait interface.
pub struct AppStateAffiliationResolver<S> {
    app_state: Arc<S>,
    mapper: PermissionMapper,
    /// Domain for JID construction (for future use)
    #[allow(dead_code)]
    domain: String,
}

impl<S> AppStateAffiliationResolver<S> {
    /// Create a new resolver with the given app state.
    pub fn new(app_state: Arc<S>, domain: String) -> Self {
        Self {
            app_state,
            mapper: PermissionMapper::new(),
            domain,
        }
    }

    /// Create a new resolver with a custom permission mapper.
    #[allow(dead_code)]
    pub fn with_mapper(app_state: Arc<S>, domain: String, mapper: PermissionMapper) -> Self {
        Self {
            app_state,
            mapper,
            domain,
        }
    }
}

impl<S> AffiliationResolver for AppStateAffiliationResolver<S>
where
    S: crate::AppState,
{
    #[instrument(skip(self), fields(user = %user_did, channel = %channel_id))]
    fn resolve_affiliation(
        &self,
        user_did: &str,
        waddle_id: &str,
        channel_id: &str,
    ) -> impl Future<Output = Result<Affiliation, XmppError>> + Send {
        let app_state = Arc::clone(&self.app_state);
        let mapper = self.mapper.clone();
        let user_did = user_did.to_string();
        let waddle_id = waddle_id.to_string();
        let channel_id = channel_id.to_string();

        async move {
            // Check permissions in order of privilege (highest first)
            // This is more efficient than checking all and taking the max
            let relations_to_check = [
                ("owner", Affiliation::Owner),
                ("admin", Affiliation::Admin),
                ("moderator", Affiliation::Admin),
                ("manager", Affiliation::Admin),
                ("member", Affiliation::Member),
                ("writer", Affiliation::Member),
                ("viewer", Affiliation::Member),
            ];

            // First check waddle-level permissions (inherit to all channels)
            for (relation, expected_affiliation) in &relations_to_check {
                let resource = format!("waddle:{}", waddle_id);
                let subject = format!("user:{}", user_did);

                match app_state
                    .check_permission(&resource, relation, &subject)
                    .await
                {
                    Ok(true) => {
                        debug!(
                            relation = %relation,
                            affiliation = %expected_affiliation,
                            "User has waddle-level permission"
                        );
                        return Ok(*expected_affiliation);
                    }
                    Ok(false) => continue,
                    Err(e) => {
                        warn!(error = %e, "Error checking waddle permission");
                        // Continue checking other permissions
                    }
                }
            }

            // Then check channel-level permissions (more specific)
            for (relation, _) in &relations_to_check {
                let resource = format!("channel:{}", channel_id);
                let subject = format!("user:{}", user_did);

                match app_state
                    .check_permission(&resource, relation, &subject)
                    .await
                {
                    Ok(true) => {
                        let affiliation = mapper.map_relation(relation);
                        debug!(
                            relation = %relation,
                            affiliation = %affiliation,
                            "User has channel-level permission"
                        );
                        return Ok(affiliation);
                    }
                    Ok(false) => continue,
                    Err(e) => {
                        warn!(error = %e, "Error checking channel permission");
                        // Continue checking other permissions
                    }
                }
            }

            debug!("User has no permissions - affiliation is None");
            Ok(Affiliation::None)
        }
    }

    #[instrument(skip(self), fields(waddle = %waddle_id, channel = %channel_id, affiliation = %affiliation))]
    fn list_affiliations(
        &self,
        waddle_id: &str,
        channel_id: &str,
        affiliation: Affiliation,
    ) -> impl Future<Output = Result<Vec<AffiliationEntry>, XmppError>> + Send {
        let app_state = Arc::clone(&self.app_state);
        let mapper = self.mapper.clone();
        let domain = self.domain.clone();
        let waddle_id = waddle_id.to_string();
        let channel_id = channel_id.to_string();

        async move {
            // Determine which relations map to this affiliation
            let relations_to_query: Vec<&str> = match affiliation {
                Affiliation::Owner => vec!["owner"],
                Affiliation::Admin => vec!["admin", "moderator", "manager"],
                Affiliation::Member => vec!["member", "writer", "viewer"],
                Affiliation::None => return Ok(Vec::new()), // No-affiliation users aren't stored
                Affiliation::Outcast => vec!["banned"],     // Banned users if we track them
            };

            let mut entries = Vec::new();

            // Query channel-level permissions first
            for relation in &relations_to_query {
                let resource = format!("channel:{}", channel_id);
                match app_state.list_subjects(&resource, relation).await {
                    Ok(subjects) => {
                        for subject_str in subjects {
                            // Parse the subject (expected format: "user:did:plc:...")
                            if let Some(did) = subject_str.strip_prefix("user:") {
                                // Convert DID to JID
                                if let Ok(jid) =
                                    format!("{}@{}", did.replace(':', "_"), domain).parse()
                                {
                                    let aff = mapper.map_relation(relation);
                                    if aff == affiliation {
                                        entries.push(AffiliationEntry::new(jid, affiliation));
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, relation = %relation, "Error listing subjects for channel");
                    }
                }
            }

            // Also query waddle-level permissions (they inherit to channels)
            for relation in &relations_to_query {
                let resource = format!("waddle:{}", waddle_id);
                match app_state.list_subjects(&resource, relation).await {
                    Ok(subjects) => {
                        for subject_str in subjects {
                            if let Some(did) = subject_str.strip_prefix("user:") {
                                if let Ok(jid) = format!("{}@{}", did.replace(':', "_"), domain)
                                    .parse::<BareJid>()
                                {
                                    // Only add if not already present (channel-level takes precedence)
                                    if !entries.iter().any(|e| e.jid == jid) {
                                        let aff = mapper.map_relation(relation);
                                        if aff == affiliation {
                                            entries.push(AffiliationEntry::new(jid, affiliation));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, relation = %relation, "Error listing subjects for waddle");
                    }
                }
            }

            debug!(count = entries.len(), "Listed affiliations");
            Ok(entries)
        }
    }

    #[instrument(skip(self), fields(user = %user_did, channel = %channel_id, members_only = %members_only))]
    fn can_join(
        &self,
        user_did: &str,
        waddle_id: &str,
        channel_id: &str,
        members_only: bool,
    ) -> impl Future<Output = Result<bool, XmppError>> + Send {
        let app_state = Arc::clone(&self.app_state);
        let user_did = user_did.to_string();
        let waddle_id = waddle_id.to_string();
        let channel_id = channel_id.to_string();

        async move {
            // For open rooms, anyone can join
            if !members_only {
                return Ok(true);
            }

            // For members-only rooms, check if user has any membership
            // Check waddle membership first (inherits to channels)
            let resource = format!("waddle:{}", waddle_id);
            let subject = format!("user:{}", user_did);

            // Check if user has view permission (minimum required)
            if app_state
                .check_permission(&resource, "view", &subject)
                .await?
            {
                return Ok(true);
            }

            // Check channel-specific membership
            let resource = format!("channel:{}", channel_id);
            if app_state
                .check_permission(&resource, "read", &subject)
                .await?
            {
                return Ok(true);
            }

            debug!("User cannot join members-only room - no membership");
            Ok(false)
        }
    }
}

/// Result of an affiliation change operation.
#[derive(Debug, Clone)]
pub struct AffiliationChange {
    /// The user whose affiliation changed
    pub jid: BareJid,
    /// Previous affiliation
    pub old_affiliation: Affiliation,
    /// New affiliation
    pub new_affiliation: Affiliation,
    /// Reason for the change
    pub reason: Option<String>,
}

impl AffiliationChange {
    /// Create a new affiliation change record.
    pub fn new(jid: BareJid, old: Affiliation, new: Affiliation) -> Self {
        Self {
            jid,
            old_affiliation: old,
            new_affiliation: new,
            reason: None,
        }
    }

    /// Check if this is an upgrade (higher privilege).
    pub fn is_upgrade(&self) -> bool {
        self.new_affiliation > self.old_affiliation
    }

    /// Check if this is a downgrade (lower privilege).
    #[allow(dead_code)]
    pub fn is_downgrade(&self) -> bool {
        self.new_affiliation < self.old_affiliation
    }
}

/// Affiliation list for a MUC room.
///
/// Stores affiliations persistently and supports sync with Zanzibar.
#[derive(Debug, Clone, Default)]
pub struct AffiliationList {
    /// Affiliations by bare JID
    affiliations: HashMap<BareJid, Affiliation>,
}

impl AffiliationList {
    /// Create an empty affiliation list.
    pub fn new() -> Self {
        Self {
            affiliations: HashMap::new(),
        }
    }

    /// Get the affiliation for a JID.
    pub fn get(&self, jid: &BareJid) -> Affiliation {
        self.affiliations
            .get(jid)
            .copied()
            .unwrap_or(Affiliation::None)
    }

    /// Set the affiliation for a JID.
    ///
    /// Returns the previous affiliation if it changed.
    pub fn set(&mut self, jid: BareJid, affiliation: Affiliation) -> Option<AffiliationChange> {
        let old = self.get(&jid);
        if old != affiliation {
            if affiliation == Affiliation::None {
                self.affiliations.remove(&jid);
            } else {
                self.affiliations.insert(jid.clone(), affiliation);
            }
            Some(AffiliationChange::new(jid, old, affiliation))
        } else {
            None
        }
    }

    /// Remove a JID from the affiliation list.
    pub fn remove(&mut self, jid: &BareJid) -> Option<AffiliationChange> {
        self.affiliations
            .remove(jid)
            .map(|old| AffiliationChange::new(jid.clone(), old, Affiliation::None))
    }

    /// Get all JIDs with a specific affiliation.
    pub fn by_affiliation(&self, affiliation: Affiliation) -> Vec<BareJid> {
        self.affiliations
            .iter()
            .filter(|(_, &a)| a == affiliation)
            .map(|(jid, _)| jid.clone())
            .collect()
    }

    /// Get all affiliation entries.
    pub fn all(&self) -> Vec<AffiliationEntry> {
        self.affiliations
            .iter()
            .map(|(jid, &affiliation)| AffiliationEntry::new(jid.clone(), affiliation))
            .collect()
    }

    /// Get the count of affiliations at each level.
    pub fn counts(&self) -> HashMap<Affiliation, usize> {
        let mut counts = HashMap::new();
        for affiliation in self.affiliations.values() {
            *counts.entry(*affiliation).or_insert(0) += 1;
        }
        counts
    }

    /// Check if a JID has at least the specified affiliation.
    pub fn has_at_least(&self, jid: &BareJid, min_affiliation: Affiliation) -> bool {
        self.get(jid) >= min_affiliation
    }

    /// Check if the list contains any owners.
    pub fn has_owner(&self) -> bool {
        self.affiliations.values().any(|&a| a == Affiliation::Owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_mapper_default_mappings() {
        let mapper = PermissionMapper::new();

        assert_eq!(mapper.map_relation("owner"), Affiliation::Owner);
        assert_eq!(mapper.map_relation("admin"), Affiliation::Admin);
        assert_eq!(mapper.map_relation("moderator"), Affiliation::Admin);
        assert_eq!(mapper.map_relation("manager"), Affiliation::Admin);
        assert_eq!(mapper.map_relation("member"), Affiliation::Member);
        assert_eq!(mapper.map_relation("writer"), Affiliation::Member);
        assert_eq!(mapper.map_relation("viewer"), Affiliation::Member);
        assert_eq!(mapper.map_relation("unknown"), Affiliation::None);
    }

    #[test]
    fn test_permission_mapper_custom_mapping() {
        let mapper = PermissionMapper::new().with_mapping("super_admin", Affiliation::Owner);

        assert_eq!(mapper.map_relation("super_admin"), Affiliation::Owner);
        assert_eq!(mapper.map_relation("admin"), Affiliation::Admin);
    }

    #[test]
    fn test_permission_mapper_highest_wins() {
        let mapper = PermissionMapper::new();

        // Multiple relations - highest should win
        let relations = vec!["member".to_string(), "admin".to_string()];
        assert_eq!(mapper.map_relations(&relations), Affiliation::Admin);

        // Empty relations
        let empty: Vec<String> = vec![];
        assert_eq!(mapper.map_relations(&empty), Affiliation::None);
    }

    #[test]
    fn test_affiliation_list_basic_operations() {
        let mut list = AffiliationList::new();
        let jid: BareJid = "user@example.com".parse().unwrap();

        // Initially no affiliation
        assert_eq!(list.get(&jid), Affiliation::None);

        // Set member
        let change = list.set(jid.clone(), Affiliation::Member);
        assert!(change.is_some());
        let change = change.unwrap();
        assert_eq!(change.old_affiliation, Affiliation::None);
        assert_eq!(change.new_affiliation, Affiliation::Member);
        assert!(change.is_upgrade());

        // Get should return member
        assert_eq!(list.get(&jid), Affiliation::Member);

        // Upgrade to admin
        let change = list.set(jid.clone(), Affiliation::Admin);
        assert!(change.is_some());
        assert!(change.unwrap().is_upgrade());

        // Setting same value should return None
        let change = list.set(jid.clone(), Affiliation::Admin);
        assert!(change.is_none());

        // Remove
        let change = list.remove(&jid);
        assert!(change.is_some());
        assert_eq!(list.get(&jid), Affiliation::None);
    }

    #[test]
    fn test_affiliation_list_by_affiliation() {
        let mut list = AffiliationList::new();

        let owner: BareJid = "owner@example.com".parse().unwrap();
        let admin1: BareJid = "admin1@example.com".parse().unwrap();
        let admin2: BareJid = "admin2@example.com".parse().unwrap();
        let member: BareJid = "member@example.com".parse().unwrap();

        list.set(owner.clone(), Affiliation::Owner);
        list.set(admin1.clone(), Affiliation::Admin);
        list.set(admin2.clone(), Affiliation::Admin);
        list.set(member.clone(), Affiliation::Member);

        let owners = list.by_affiliation(Affiliation::Owner);
        assert_eq!(owners.len(), 1);
        assert!(owners.contains(&owner));

        let admins = list.by_affiliation(Affiliation::Admin);
        assert_eq!(admins.len(), 2);
        assert!(admins.contains(&admin1));
        assert!(admins.contains(&admin2));

        let members = list.by_affiliation(Affiliation::Member);
        assert_eq!(members.len(), 1);
        assert!(members.contains(&member));
    }

    #[test]
    fn test_affiliation_list_has_at_least() {
        let mut list = AffiliationList::new();
        let jid: BareJid = "user@example.com".parse().unwrap();

        list.set(jid.clone(), Affiliation::Admin);

        assert!(list.has_at_least(&jid, Affiliation::Member));
        assert!(list.has_at_least(&jid, Affiliation::Admin));
        assert!(!list.has_at_least(&jid, Affiliation::Owner));
    }

    #[test]
    fn test_affiliation_list_has_owner() {
        let mut list = AffiliationList::new();

        let admin: BareJid = "admin@example.com".parse().unwrap();
        list.set(admin, Affiliation::Admin);
        assert!(!list.has_owner());

        let owner: BareJid = "owner@example.com".parse().unwrap();
        list.set(owner, Affiliation::Owner);
        assert!(list.has_owner());
    }

    #[test]
    fn test_affiliation_list_counts() {
        let mut list = AffiliationList::new();

        list.set("owner@example.com".parse().unwrap(), Affiliation::Owner);
        list.set("admin1@example.com".parse().unwrap(), Affiliation::Admin);
        list.set("admin2@example.com".parse().unwrap(), Affiliation::Admin);
        list.set("member1@example.com".parse().unwrap(), Affiliation::Member);
        list.set("member2@example.com".parse().unwrap(), Affiliation::Member);
        list.set("member3@example.com".parse().unwrap(), Affiliation::Member);

        let counts = list.counts();
        assert_eq!(counts.get(&Affiliation::Owner), Some(&1));
        assert_eq!(counts.get(&Affiliation::Admin), Some(&2));
        assert_eq!(counts.get(&Affiliation::Member), Some(&3));
        assert_eq!(counts.get(&Affiliation::None), None);
    }

    // =============================================================================
    // Federated Permission Policy Tests
    // =============================================================================

    #[test]
    fn test_federated_permission_policy_default() {
        let policy = FederatedPermissionPolicy::default();
        assert_eq!(policy, FederatedPermissionPolicy::Open);
        assert!(policy.allows_federation());
        assert!(policy.is_open());
    }

    #[test]
    fn test_federated_permission_policy_helpers() {
        assert!(FederatedPermissionPolicy::Open.allows_federation());
        assert!(FederatedPermissionPolicy::Open.is_open());
        assert!(!FederatedPermissionPolicy::Open.uses_allow_list());
        assert!(!FederatedPermissionPolicy::Open.uses_block_list());

        assert!(FederatedPermissionPolicy::AllowList.allows_federation());
        assert!(!FederatedPermissionPolicy::AllowList.is_open());
        assert!(FederatedPermissionPolicy::AllowList.uses_allow_list());
        assert!(!FederatedPermissionPolicy::AllowList.uses_block_list());

        assert!(FederatedPermissionPolicy::BlockList.allows_federation());
        assert!(!FederatedPermissionPolicy::BlockList.is_open());
        assert!(!FederatedPermissionPolicy::BlockList.uses_allow_list());
        assert!(FederatedPermissionPolicy::BlockList.uses_block_list());

        assert!(!FederatedPermissionPolicy::Closed.allows_federation());
        assert!(!FederatedPermissionPolicy::Closed.is_open());
        assert!(!FederatedPermissionPolicy::Closed.uses_allow_list());
        assert!(!FederatedPermissionPolicy::Closed.uses_block_list());
    }

    #[test]
    fn test_federated_affiliation_config_defaults() {
        let config = FederatedAffiliationConfig::default();
        assert_eq!(config.default_affiliation, Affiliation::None);
        assert!(config.allowed_domains.is_empty());
        assert!(config.blocked_domains.is_empty());

        let open_member = FederatedAffiliationConfig::open_member();
        assert_eq!(open_member.default_affiliation, Affiliation::Member);

        let open_none = FederatedAffiliationConfig::open_none();
        assert_eq!(open_none.default_affiliation, Affiliation::None);
    }

    #[test]
    fn test_federated_affiliation_config_domain_lists() {
        let mut config = FederatedAffiliationConfig::new(Affiliation::Member);

        // Add allowed domains
        config.add_allowed_domain("trusted.example.com");
        config.add_allowed_domain("partner.example.org");
        assert!(config.is_domain_allowed("trusted.example.com"));
        assert!(config.is_domain_allowed("partner.example.org"));
        assert!(!config.is_domain_allowed("unknown.example.net"));

        // Remove allowed domain
        assert!(config.remove_allowed_domain("trusted.example.com"));
        assert!(!config.is_domain_allowed("trusted.example.com"));
        assert!(!config.remove_allowed_domain("nonexistent.example.com"));

        // Add blocked domains
        config.add_blocked_domain("spam.example.com");
        assert!(config.is_domain_blocked("spam.example.com"));
        assert!(!config.is_domain_blocked("good.example.com"));

        // Remove blocked domain
        assert!(config.remove_blocked_domain("spam.example.com"));
        assert!(!config.is_domain_blocked("spam.example.com"));
    }

    #[test]
    fn test_federated_affiliation_config_jid_lists() {
        let mut config = FederatedAffiliationConfig::new(Affiliation::Member);

        let allowed_jid: BareJid = "allowed@trusted.example.com".parse().unwrap();
        let blocked_jid: BareJid = "spammer@spam.example.com".parse().unwrap();
        let unknown_jid: BareJid = "unknown@example.com".parse().unwrap();

        // Add allowed JID
        config.add_allowed_jid(allowed_jid.clone());
        assert!(config.is_jid_allowed(&allowed_jid));
        assert!(!config.is_jid_allowed(&unknown_jid));

        // Add blocked JID
        config.add_blocked_jid(blocked_jid.clone());
        assert!(config.is_jid_blocked(&blocked_jid));
        assert!(!config.is_jid_blocked(&unknown_jid));

        // Remove JIDs
        assert!(config.remove_allowed_jid(&allowed_jid));
        assert!(!config.is_jid_allowed(&allowed_jid));
        assert!(config.remove_blocked_jid(&blocked_jid));
        assert!(!config.is_jid_blocked(&blocked_jid));
    }

    #[test]
    fn test_federated_affiliation_config_affiliation_overrides() {
        let mut config = FederatedAffiliationConfig::new(Affiliation::Member);

        let user_jid: BareJid = "user@partner.example.org".parse().unwrap();
        let admin_jid: BareJid = "admin@partner.example.org".parse().unwrap();
        let normal_jid: BareJid = "user@other.example.com".parse().unwrap();

        // Set domain-level override
        config.set_domain_affiliation("partner.example.org", Affiliation::Admin);

        // Set JID-level override (takes precedence)
        config.set_jid_affiliation(admin_jid.clone(), Affiliation::Owner);

        // Check affiliation resolution priority:
        // 1. JID-specific override wins
        assert_eq!(
            config.get_affiliation_for_jid(&admin_jid),
            Affiliation::Owner
        );
        // 2. Domain-specific override
        assert_eq!(
            config.get_affiliation_for_jid(&user_jid),
            Affiliation::Admin
        );
        // 3. Default affiliation
        assert_eq!(
            config.get_affiliation_for_jid(&normal_jid),
            Affiliation::Member
        );

        // Remove overrides
        assert_eq!(
            config.remove_jid_affiliation(&admin_jid),
            Some(Affiliation::Owner)
        );
        assert_eq!(
            config.get_affiliation_for_jid(&admin_jid),
            Affiliation::Admin
        );

        assert_eq!(
            config.remove_domain_affiliation("partner.example.org"),
            Some(Affiliation::Admin)
        );
        assert_eq!(
            config.get_affiliation_for_jid(&user_jid),
            Affiliation::Member
        );
    }

    #[test]
    fn test_federated_policy_open_allows_all() {
        let config = FederatedAffiliationConfig::open_member();

        let jid1: BareJid = "user@server1.example.com".parse().unwrap();
        let jid2: BareJid = "user@server2.example.org".parse().unwrap();
        let jid3: BareJid = "user@any.domain.net".parse().unwrap();

        // Open policy allows all JIDs
        assert!(config.is_allowed_by_policy(&jid1, FederatedPermissionPolicy::Open));
        assert!(config.is_allowed_by_policy(&jid2, FederatedPermissionPolicy::Open));
        assert!(config.is_allowed_by_policy(&jid3, FederatedPermissionPolicy::Open));
    }

    #[test]
    fn test_federated_policy_closed_blocks_all() {
        let config = FederatedAffiliationConfig::open_member();

        let jid1: BareJid = "user@server1.example.com".parse().unwrap();
        let jid2: BareJid = "user@server2.example.org".parse().unwrap();

        // Closed policy blocks all JIDs
        assert!(!config.is_allowed_by_policy(&jid1, FederatedPermissionPolicy::Closed));
        assert!(!config.is_allowed_by_policy(&jid2, FederatedPermissionPolicy::Closed));
    }

    #[test]
    fn test_federated_policy_allowlist_domain() {
        let mut config = FederatedAffiliationConfig::new(Affiliation::Member);
        config.add_allowed_domain("trusted.example.com");
        config.add_allowed_domain("partner.example.org");

        let allowed_jid: BareJid = "user@trusted.example.com".parse().unwrap();
        let partner_jid: BareJid = "user@partner.example.org".parse().unwrap();
        let blocked_jid: BareJid = "user@unknown.example.net".parse().unwrap();

        // AllowList policy: only allowed domains pass
        assert!(config.is_allowed_by_policy(&allowed_jid, FederatedPermissionPolicy::AllowList));
        assert!(config.is_allowed_by_policy(&partner_jid, FederatedPermissionPolicy::AllowList));
        assert!(!config.is_allowed_by_policy(&blocked_jid, FederatedPermissionPolicy::AllowList));
    }

    #[test]
    fn test_federated_policy_allowlist_jid() {
        let mut config = FederatedAffiliationConfig::new(Affiliation::Member);
        // Don't add any allowed domains

        let specific_jid: BareJid = "special@any.example.com".parse().unwrap();
        let other_jid: BareJid = "other@any.example.com".parse().unwrap();

        // Add specific JID to allow list
        config.add_allowed_jid(specific_jid.clone());

        // AllowList: JID-specific allows work even if domain isn't allowed
        assert!(config.is_allowed_by_policy(&specific_jid, FederatedPermissionPolicy::AllowList));
        assert!(!config.is_allowed_by_policy(&other_jid, FederatedPermissionPolicy::AllowList));
    }

    #[test]
    fn test_federated_policy_blocklist_domain() {
        let mut config = FederatedAffiliationConfig::new(Affiliation::Member);
        config.add_blocked_domain("spam.example.com");
        config.add_blocked_domain("abuse.example.org");

        let blocked_jid1: BareJid = "user@spam.example.com".parse().unwrap();
        let blocked_jid2: BareJid = "user@abuse.example.org".parse().unwrap();
        let allowed_jid: BareJid = "user@good.example.net".parse().unwrap();

        // BlockList policy: blocked domains are rejected, others pass
        assert!(!config.is_allowed_by_policy(&blocked_jid1, FederatedPermissionPolicy::BlockList));
        assert!(!config.is_allowed_by_policy(&blocked_jid2, FederatedPermissionPolicy::BlockList));
        assert!(config.is_allowed_by_policy(&allowed_jid, FederatedPermissionPolicy::BlockList));
    }

    #[test]
    fn test_federated_policy_blocklist_jid() {
        let mut config = FederatedAffiliationConfig::new(Affiliation::Member);

        let bad_user: BareJid = "baduser@good.example.com".parse().unwrap();
        let good_user: BareJid = "gooduser@good.example.com".parse().unwrap();

        // Block specific JID
        config.add_blocked_jid(bad_user.clone());

        // BlockList: JID-specific blocks work even if domain is allowed
        assert!(!config.is_allowed_by_policy(&bad_user, FederatedPermissionPolicy::BlockList));
        assert!(config.is_allowed_by_policy(&good_user, FederatedPermissionPolicy::BlockList));
    }

    #[test]
    fn test_federated_policy_blocklist_jid_overrides_domain_block() {
        let mut config = FederatedAffiliationConfig::new(Affiliation::Member);

        // Block the domain
        config.add_blocked_domain("mostly-bad.example.com");

        // But allow one specific JID from that domain
        let exception_jid: BareJid = "good-user@mostly-bad.example.com".parse().unwrap();
        let regular_jid: BareJid = "regular@mostly-bad.example.com".parse().unwrap();
        config.add_allowed_jid(exception_jid.clone());

        // BlockList: JID allow list overrides domain block
        assert!(config.is_allowed_by_policy(&exception_jid, FederatedPermissionPolicy::BlockList));
        assert!(!config.is_allowed_by_policy(&regular_jid, FederatedPermissionPolicy::BlockList));
    }

    #[test]
    fn test_federated_user_join_open_room_open_policy() {
        let config = FederatedAffiliationConfig::open_member();
        let jid: BareJid = "user@remote.example.com".parse().unwrap();

        // Open room, open policy: anyone can join
        let affiliation = config.get_affiliation_for_jid(&jid);
        assert_eq!(affiliation, Affiliation::Member);

        let can_join = config.is_allowed_by_policy(&jid, FederatedPermissionPolicy::Open)
            && affiliation != Affiliation::Outcast;
        assert!(can_join);
    }

    #[test]
    fn test_federated_user_join_members_only_room() {
        let config = FederatedAffiliationConfig::open_member();
        let jid: BareJid = "user@remote.example.com".parse().unwrap();

        // Members-only room: federated users with Member affiliation can join
        let affiliation = config.get_affiliation_for_jid(&jid);
        assert!(affiliation >= Affiliation::Member);

        // With None affiliation, cannot join members-only
        let none_config = FederatedAffiliationConfig::open_none();
        let none_affiliation = none_config.get_affiliation_for_jid(&jid);
        assert!(none_affiliation < Affiliation::Member);
    }

    #[test]
    fn test_federated_user_join_closed_policy() {
        let config = FederatedAffiliationConfig::open_member();
        let jid: BareJid = "user@remote.example.com".parse().unwrap();

        // Closed policy: no one can join regardless of affiliation
        assert!(!config.is_allowed_by_policy(&jid, FederatedPermissionPolicy::Closed));
    }

    #[test]
    fn test_federated_user_join_blocked_domain() {
        let mut config = FederatedAffiliationConfig::open_member();
        config.add_blocked_domain("spam.example.com");

        let blocked_jid: BareJid = "user@spam.example.com".parse().unwrap();
        let allowed_jid: BareJid = "user@good.example.com".parse().unwrap();

        // BlockList policy: blocked domain rejected
        assert!(!config.is_allowed_by_policy(&blocked_jid, FederatedPermissionPolicy::BlockList));
        assert!(config.is_allowed_by_policy(&allowed_jid, FederatedPermissionPolicy::BlockList));
    }

    #[test]
    fn test_federated_user_join_allowlist_with_accepted_domain() {
        let mut config = FederatedAffiliationConfig::new(Affiliation::Member);
        config.add_allowed_domain("trusted.example.com");

        let allowed_jid: BareJid = "user@trusted.example.com".parse().unwrap();
        let rejected_jid: BareJid = "user@untrusted.example.org".parse().unwrap();

        // AllowList policy: only allowed domain passes
        assert!(config.is_allowed_by_policy(&allowed_jid, FederatedPermissionPolicy::AllowList));
        assert!(!config.is_allowed_by_policy(&rejected_jid, FederatedPermissionPolicy::AllowList));
    }
}
