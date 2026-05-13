//! XMPP AppState implementation bridging to waddle-server services.
//!
//! This module implements the `waddle_xmpp::AppState` trait by delegating to
//! the existing auth/session services and PermissionActor-backed permission
//! actor in waddle-server.

#[cfg(test)]
use waddle_xmpp::inbox::InboxEntry;
#[cfg(test)]
use waddle_xmpp::{Session as XmppSession, XmppError};

#[cfg(test)]
pub use super::xmpp_app_state::XmppAppState;

pub(crate) use super::xmpp_channels::{get_xmpp_channel, list_xmpp_channels, XmppChannelRecord};

#[cfg(test)]
impl waddle_xmpp::AppState for XmppAppState {
    /// Validate an XMPP session token and return the associated session.
    ///
    /// The token is expected to be a session ID from the HTTP authentication flow.
    /// The JID's localpart is verified against the immutable session localpart.
    async fn validate_session(
        &self,
        jid: &jid::Jid,
        token: &str,
    ) -> Result<XmppSession, XmppError> {
        super::xmpp_auth_state::validate_session(&self.session_manager, jid, token).await
    }

    /// Check if a subject has permission to perform an action on a resource.
    ///
    /// Resource format: "space:{id}" or "channel:{id}"
    /// Subject format: "user:{user_id}"
    async fn check_permission(
        &self,
        resource: &str,
        action: &str,
        subject: &str,
    ) -> Result<bool, XmppError> {
        super::xmpp_permission_state::check_permission(
            &self.permission_actor,
            resource,
            action,
            subject,
        )
        .await
    }

    /// Validate an XMPP session token without a JID (for OAUTHBEARER).
    ///
    /// The token is expected to be a session ID. The JID is derived from the
    /// session's immutable localpart after validation.
    async fn validate_session_token(&self, token: &str) -> Result<XmppSession, XmppError> {
        super::xmpp_auth_state::validate_session_token(&self.session_manager, &self.domain, token)
            .await
    }

    /// Get the XMPP server domain.
    fn domain(&self) -> &str {
        &self.domain
    }

    /// Get the OAuth discovery URL for XMPP OAUTHBEARER (XEP-0493).
    ///
    /// Returns the RFC 8414 OAuth authorization server metadata endpoint URL.
    fn oauth_discovery_url(&self) -> String {
        super::xmpp_auth_state::oauth_discovery_url(&self.domain)
    }

    /// List all relations a subject has on an object.
    ///
    /// Used for deriving MUC affiliations from multiple permission relations.
    async fn list_relations(
        &self,
        resource: &str,
        subject: &str,
    ) -> Result<Vec<String>, XmppError> {
        super::xmpp_permission_state::list_relations(&self.permission_actor, resource, subject)
            .await
    }

    /// List all subjects with a specific relation on an object.
    ///
    /// Used for MUC affiliation list queries (XEP-0045).
    async fn list_subjects(
        &self,
        resource: &str,
        relation: &str,
    ) -> Result<Vec<String>, XmppError> {
        super::xmpp_permission_state::list_subjects(&self.permission_actor, resource, relation)
            .await
    }

    async fn resolve_subject_jid(&self, subject: &str) -> Result<Option<jid::BareJid>, XmppError> {
        super::xmpp_permission_state::resolve_subject_jid(
            &self.global_db_actor,
            &self.domain,
            subject,
        )
        .await
    }

    async fn search_users(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<waddle_xmpp::UserDirectoryEntry>, XmppError> {
        super::xmpp_account_state::search_users(&self.global_db_actor, &self.domain, query, limit)
            .await
    }

    async fn set_room_affiliation(
        &self,
        channel_id: &str,
        jid: &jid::BareJid,
        affiliation: waddle_xmpp::Affiliation,
    ) -> Result<(), XmppError> {
        super::xmpp_permission_state::set_room_affiliation(
            &self.global_db_actor,
            &self.permission_actor,
            channel_id,
            jid,
            affiliation,
        )
        .await
    }

    /// Lookup SCRAM credentials for a native JID user.
    ///
    /// Queries the NativeUserStore for SCRAM credentials if the user exists.
    /// Returns None if the user doesn't exist or native auth is not available.
    async fn lookup_scram_credentials(
        &self,
        username: &str,
    ) -> Result<Option<waddle_xmpp::ScramCredentials>, XmppError> {
        super::xmpp_account_state::lookup_scram_credentials(
            &self.native_user_store,
            &self.domain,
            username,
        )
        .await
    }

    /// Register a new native user via XEP-0077 In-Band Registration.
    ///
    /// Creates a new user with securely hashed password and SCRAM keys.
    async fn register_native_user(
        &self,
        username: &str,
        password: &str,
        email: Option<&str>,
    ) -> Result<(), XmppError> {
        super::xmpp_account_state::register_native_user(
            &self.native_user_store,
            &self.permission_actor,
            &self.domain,
            username,
            password,
            email,
        )
        .await
    }

    /// Check if a native user exists.
    async fn native_user_exists(&self, username: &str) -> Result<bool, XmppError> {
        super::xmpp_account_state::native_user_exists(
            &self.native_user_store,
            &self.domain,
            username,
        )
        .await
    }

    /// Get the vCard for a user (XEP-0054).
    async fn get_vcard(
        &self,
        jid: &jid::BareJid,
    ) -> Result<Option<xmpp_parsers::minidom::Element>, XmppError> {
        super::xmpp_profile_state::get_vcard(&self.vcard_store, jid).await
    }

    /// Store/update the vCard for a user (XEP-0054).
    async fn set_vcard(
        &self,
        jid: &jid::BareJid,
        vcard: &xmpp_parsers::minidom::Element,
    ) -> Result<(), XmppError> {
        super::xmpp_profile_state::set_vcard(&self.vcard_store, jid, vcard).await
    }

    /// Look up the externally-hosted avatar URL for a JID (XEP-0084 `url=`).
    ///
    /// Reads `users.avatar_url` keyed on `xmpp_localpart`. The URL is the one
    /// captured during OIDC login (e.g. a GitHub avatar). Missing or empty
    /// values return `Ok(None)`.
    async fn get_user_avatar_url(&self, jid: &jid::BareJid) -> Result<Option<String>, XmppError> {
        super::xmpp_profile_state::get_user_avatar_url(&self.global_db_actor, jid).await
    }

    /// Create an upload slot for XEP-0363 HTTP File Upload.
    async fn create_upload_slot(
        &self,
        requester_jid: &jid::BareJid,
        filename: &str,
        size: u64,
        content_type: Option<&str>,
    ) -> Result<waddle_xmpp::UploadSlotInfo, XmppError> {
        super::xmpp_upload_state::create_upload_slot(
            &self.global_db_actor,
            &self.domain,
            requester_jid,
            filename,
            size,
            content_type,
        )
        .await
    }

    /// Get the maximum allowed file upload size in bytes.
    fn max_upload_size(&self) -> u64 {
        super::routes::uploads::max_upload_size()
    }

    // =========================================================================
    // RFC 6121 Roster Storage Methods
    // =========================================================================

    /// Get all roster items for a user.
    async fn get_roster(
        &self,
        user_jid: &jid::BareJid,
    ) -> Result<Vec<waddle_xmpp::roster::RosterItem>, XmppError> {
        super::xmpp_roster_state::get_roster(self.global_database().await?, user_jid).await
    }

    /// Get a single roster item by JID.
    async fn get_roster_item(
        &self,
        user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
    ) -> Result<Option<waddle_xmpp::roster::RosterItem>, XmppError> {
        super::xmpp_roster_state::get_roster_item(
            self.global_database().await?,
            user_jid,
            contact_jid,
        )
        .await
    }

    /// Add or update a roster item.
    async fn set_roster_item(
        &self,
        user_jid: &jid::BareJid,
        item: &waddle_xmpp::roster::RosterItem,
    ) -> Result<waddle_xmpp::roster::RosterSetResult, XmppError> {
        super::xmpp_roster_state::set_roster_item(self.global_database().await?, user_jid, item)
            .await
    }

    /// Remove a roster item.
    async fn remove_roster_item(
        &self,
        user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
    ) -> Result<bool, XmppError> {
        super::xmpp_roster_state::remove_roster_item(
            self.global_database().await?,
            user_jid,
            contact_jid,
        )
        .await
    }

    /// Get the current roster version for a user.
    async fn get_roster_version(
        &self,
        user_jid: &jid::BareJid,
    ) -> Result<Option<String>, XmppError> {
        super::xmpp_roster_state::get_roster_version(self.global_database().await?, user_jid).await
    }

    /// Update the subscription state for a roster item.
    async fn update_roster_subscription(
        &self,
        user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
        subscription: waddle_xmpp::roster::Subscription,
        ask: Option<waddle_xmpp::roster::AskType>,
    ) -> Result<waddle_xmpp::roster::RosterItem, XmppError> {
        super::xmpp_roster_state::update_roster_subscription(
            self.global_database().await?,
            user_jid,
            contact_jid,
            subscription,
            ask,
        )
        .await
    }

    /// Get all roster items where the user should send presence updates.
    async fn get_presence_subscribers(
        &self,
        user_jid: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, XmppError> {
        super::xmpp_roster_state::get_presence_subscribers(self.global_database().await?, user_jid)
            .await
    }

    /// Get all roster items where the user receives presence updates.
    async fn get_presence_subscriptions(
        &self,
        user_jid: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, XmppError> {
        super::xmpp_roster_state::get_presence_subscriptions(
            self.global_database().await?,
            user_jid,
        )
        .await
    }

    // =========================================================================
    // XEP-0191 Blocking Command Methods
    // =========================================================================

    /// Get all blocked JIDs for a user.
    async fn get_blocklist(&self, user_jid: &jid::BareJid) -> Result<Vec<String>, XmppError> {
        super::xmpp_user_storage_state::get_blocklist(self.global_database().await?, user_jid).await
    }

    /// Check if a JID is blocked by a user.
    async fn is_blocked(
        &self,
        user_jid: &jid::BareJid,
        blocked_jid: &jid::BareJid,
    ) -> Result<bool, XmppError> {
        super::xmpp_user_storage_state::is_blocked(
            self.global_database().await?,
            user_jid,
            blocked_jid,
        )
        .await
    }

    /// Add JIDs to a user's blocklist.
    async fn add_blocks(
        &self,
        user_jid: &jid::BareJid,
        blocked_jids: &[String],
    ) -> Result<usize, XmppError> {
        super::xmpp_user_storage_state::add_blocks(
            self.global_database().await?,
            user_jid,
            blocked_jids,
        )
        .await
    }

    /// Remove JIDs from a user's blocklist.
    async fn remove_blocks(
        &self,
        user_jid: &jid::BareJid,
        blocked_jids: &[String],
    ) -> Result<usize, XmppError> {
        super::xmpp_user_storage_state::remove_blocks(
            self.global_database().await?,
            user_jid,
            blocked_jids,
        )
        .await
    }

    /// Remove all JIDs from a user's blocklist.
    async fn remove_all_blocks(&self, user_jid: &jid::BareJid) -> Result<usize, XmppError> {
        super::xmpp_user_storage_state::remove_all_blocks(self.global_database().await?, user_jid)
            .await
    }

    // =========================================================================
    // XEP-0049 Private XML Storage Methods
    // =========================================================================

    /// Get private XML data for a user by namespace.
    async fn get_private_xml(
        &self,
        jid: &jid::BareJid,
        namespace: &str,
    ) -> Result<Option<String>, XmppError> {
        super::xmpp_user_storage_state::get_private_xml(&self.global_db_actor, jid, namespace).await
    }

    /// Store/update private XML data for a user by namespace.
    async fn set_private_xml(
        &self,
        jid: &jid::BareJid,
        namespace: &str,
        xml_content: &str,
    ) -> Result<(), XmppError> {
        super::xmpp_user_storage_state::set_private_xml(
            &self.global_db_actor,
            jid,
            namespace,
            xml_content,
        )
        .await
    }

    // =========================================================================
    // Waddle Inbox Projection Methods
    // =========================================================================

    async fn list_inbox(&self, user_jid: &jid::BareJid) -> Result<Vec<InboxEntry>, XmppError> {
        super::xmpp_user_storage_state::list_inbox(self.inbox_storage.as_deref(), user_jid).await
    }

    async fn upsert_inbox_entry(
        &self,
        user_jid: &jid::BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<(), XmppError> {
        super::xmpp_user_storage_state::upsert_inbox_entry(
            self.inbox_storage.as_deref(),
            user_jid,
            entry,
            increment_unread,
        )
        .await
    }

    async fn mark_inbox_read(
        &self,
        user_jid: &jid::BareJid,
        partner_jid: &jid::BareJid,
    ) -> Result<(), XmppError> {
        super::xmpp_user_storage_state::mark_inbox_read(
            self.inbox_storage.as_deref(),
            user_jid,
            partner_jid,
        )
        .await
    }

    async fn inbox_total_unread(&self, user_jid: &jid::BareJid) -> Result<u64, XmppError> {
        super::xmpp_user_storage_state::inbox_total_unread(self.inbox_storage.as_deref(), user_jid)
            .await
    }

    // =========================================================================
    // Auto-Join: Space & Channel Enumeration
    // =========================================================================

    /// List all channels in the canonical space.
    async fn list_space_channels(&self) -> Result<Vec<waddle_xmpp::ChannelInfo>, XmppError> {
        super::xmpp_space_state::list_space_channels(self.space_db_actor.as_ref()).await
    }

    /// Look up a channel-backed room by channel ID.
    async fn get_channel_room_info(
        &self,
        channel_id: &str,
    ) -> Result<Option<waddle_xmpp::ChannelRoomInfo>, XmppError> {
        super::xmpp_space_state::get_channel_room_info(self.space_db_actor.as_ref(), channel_id)
            .await
    }

    // =========================================================================
    // XEP-0503: Spaces Service
    // =========================================================================

    /// Get detailed information about the canonical space.
    async fn get_space_details(&self) -> Result<Option<waddle_xmpp::SpaceDetails>, XmppError> {
        Ok(Some(super::xmpp_space_state::get_space_details(
            &self.domain,
        )))
    }
}

#[cfg(test)]
#[path = "xmpp_state_tests.rs"]
mod tests;
