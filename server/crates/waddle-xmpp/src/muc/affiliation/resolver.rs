use std::future::Future;
use std::sync::Arc;

use jid::BareJid;
use tracing::{debug, instrument, warn};

use crate::types::Affiliation;
use crate::XmppError;

use super::{AffiliationEntry, FederatedAffiliationConfig, FederatedPermissionPolicy};

/// Trait for resolving MUC affiliations from external permission systems.
///
/// Implement this trait to connect the XMPP server to different
/// permission backends (Zanzibar, RBAC, etc.).
pub trait AffiliationResolver: Send + Sync {
    /// Resolve the affiliation for a user in a channel.
    ///
    /// # Arguments
    /// * `user_id` - The user identifier used for permission subjects.
    /// * `waddle_id` - The Waddle community ID
    /// * `channel_id` - The channel ID within the Waddle
    ///
    /// # Returns
    /// The MUC affiliation for the user, or an error if resolution fails.
    fn resolve_affiliation(
        &self,
        user_id: &str,
        _waddle_id: &str,
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
        user_id: &str,
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
        if !config.is_allowed_by_policy(jid, policy) {
            return Affiliation::None;
        }

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
        if !config.is_allowed_by_policy(jid, policy) {
            return false;
        }

        let affiliation = config.get_affiliation_for_jid(jid);

        if members_only {
            affiliation >= Affiliation::Member
        } else {
            affiliation != Affiliation::Outcast
        }
    }
}

/// Affiliation resolver that uses the AppState's check_permission method.
///
/// This resolver queries the Zanzibar permission system through the
/// AppState trait interface.
pub struct AppStateAffiliationResolver<S> {
    app_state: Arc<S>,
}

impl<S> AppStateAffiliationResolver<S> {
    /// Create a new resolver with the given app state.
    pub fn new(app_state: Arc<S>, _domain: String) -> Self {
        Self { app_state }
    }
}

impl<S> AffiliationResolver for AppStateAffiliationResolver<S>
where
    S: crate::AppState,
{
    #[instrument(skip(self), fields(user = %user_id, channel = %channel_id))]
    fn resolve_affiliation(
        &self,
        user_id: &str,
        waddle_id: &str,
        channel_id: &str,
    ) -> impl Future<Output = Result<Affiliation, XmppError>> + Send {
        let app_state = Arc::clone(&self.app_state);
        let user_id = user_id.to_string();
        let waddle_id = waddle_id.to_string();
        let channel_id = channel_id.to_string();

        async move {
            let subject = format!("user:{}", user_id);
            let channel_resource = format!("channel:{}", channel_id);
            if app_state
                .check_permission(&channel_resource, "outcast", &subject)
                .await?
            {
                return Ok(Affiliation::Outcast);
            }

            let relations_to_check = [
                ("owner", Affiliation::Owner),
                ("admin", Affiliation::Admin),
                ("member", Affiliation::Member),
            ];

            let resources = [
                (
                    format!("space:{}", waddle_id),
                    "space-level MUC affiliation",
                ),
                (channel_resource, "channel-level MUC affiliation"),
            ];

            for (resource, scope) in resources {
                for (relation, affiliation) in &relations_to_check {
                    match app_state
                        .check_permission(&resource, relation, &subject)
                        .await
                    {
                        Ok(true) => {
                            debug!(
                                relation = %relation,
                                affiliation = %affiliation,
                                scope = %scope,
                                "User has MUC affiliation"
                            );
                            return Ok(*affiliation);
                        }
                        Ok(false) => continue,
                        Err(e) => {
                            warn!(error = %e, resource = %resource, "Error checking MUC affiliation");
                        }
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
        let waddle_id = waddle_id.to_string();
        let channel_id = channel_id.to_string();

        async move {
            let relations_to_query: Vec<&str> = match affiliation {
                Affiliation::Owner => vec!["owner"],
                Affiliation::Admin => vec!["admin"],
                Affiliation::Member => vec!["member"],
                Affiliation::None => return Ok(Vec::new()),
                Affiliation::Outcast => vec!["outcast"],
            };

            let mut entries = Vec::new();

            let resources = [
                format!("space:{}", waddle_id),
                format!("channel:{}", channel_id),
            ];

            for resource in resources {
                for relation in &relations_to_query {
                    match app_state.list_subjects(&resource, relation).await {
                        Ok(subjects) => {
                            for subject_str in subjects {
                                match app_state.resolve_subject_jid(&subject_str).await {
                                    Ok(Some(jid)) => {
                                        entries.push(AffiliationEntry::new(jid, affiliation));
                                    }
                                    Ok(None) => {}
                                    Err(e) => {
                                        warn!(error = %e, subject = %subject_str, "Error resolving subject JID");
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, relation = %relation, resource = %resource, "Error listing subjects for MUC affiliation");
                        }
                    }
                }
            }

            entries.sort_by_key(|entry| entry.jid.to_string());
            entries.dedup_by(|a, b| a.jid == b.jid);
            debug!(count = entries.len(), "Listed affiliations");
            Ok(entries)
        }
    }

    #[instrument(skip(self), fields(user = %user_id, channel = %channel_id, members_only = %members_only))]
    fn can_join(
        &self,
        user_id: &str,
        waddle_id: &str,
        channel_id: &str,
        members_only: bool,
    ) -> impl Future<Output = Result<bool, XmppError>> + Send {
        let app_state = Arc::clone(&self.app_state);
        let user_id = user_id.to_string();
        let waddle_id = waddle_id.to_string();
        let channel_id = channel_id.to_string();

        async move {
            // For open rooms, anyone can join
            if !members_only {
                return Ok(true);
            }

            let subject = format!("user:{}", user_id);
            let channel_resource = format!("channel:{}", channel_id);

            if app_state
                .check_permission(&channel_resource, "outcast", &subject)
                .await?
            {
                return Ok(false);
            }

            let space_resource = format!("space:{}", waddle_id);
            if app_state
                .check_permission(&space_resource, "view", &subject)
                .await?
            {
                return Ok(true);
            }

            if app_state
                .check_permission(&channel_resource, "read", &subject)
                .await?
            {
                return Ok(true);
            }

            debug!("User cannot join members-only room - no membership");
            Ok(false)
        }
    }
}
