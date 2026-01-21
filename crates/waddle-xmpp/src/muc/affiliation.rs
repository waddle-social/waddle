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

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use jid::BareJid;
use tracing::{debug, instrument, warn};

use crate::types::Affiliation;
use crate::XmppError;

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

                match app_state.check_permission(&resource, relation, &subject).await {
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

                match app_state.check_permission(&resource, relation, &subject).await {
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

    async fn list_affiliations(
        &self,
        _waddle_id: &str,
        _channel_id: &str,
        _affiliation: Affiliation,
    ) -> Result<Vec<AffiliationEntry>, XmppError> {
        // This requires a list_subjects query which isn't exposed through AppState
        // For now, return an empty list - this would need to be implemented
        // when the permission service is directly accessible
        // TODO: Implement when we have direct access to TupleStore.list_subjects
        Ok(Vec::new())
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
        self.affiliations.get(jid).copied().unwrap_or(Affiliation::None)
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
        let mapper =
            PermissionMapper::new().with_mapping("super_admin", Affiliation::Owner);

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
}
