//! XMPP AppState implementation bridging to waddle-server services.
//!
//! This module implements the `waddle_xmpp::AppState` trait by delegating to
//! the existing auth, session, and permission services in waddle-server.

#[cfg(test)]
use tracing::{debug, warn};
#[cfg(test)]
use waddle_xmpp::inbox::{InboxEntry, storage::InboxStorage};
#[cfg(test)]
use waddle_xmpp::{Session as XmppSession, UserDirectoryEntry, XmppError};

#[cfg(test)]
use crate::auth::jid::jid_to_localpart;
#[cfg(test)]
use crate::auth::{NativeUserStore, RegisterRequest, SessionManager, localpart_to_jid};
#[cfg(test)]
use crate::db::Database;
#[cfg(test)]
use crate::db::actor::{DbActor, DbQuery, DbQueryOne};
#[cfg(test)]
use crate::db::actor::{DbExecute, GetDatabase};
#[cfg(test)]
use crate::db::{ValueExt, row_value};
#[cfg(test)]
use crate::permissions::{Object, PermissionActor, Subject};
#[cfg(test)]
use crate::vcard::VCardStore;
#[cfg(test)]
use kameo::actor::ActorRef;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use crate::server::bootstrap_membership::{BootstrapMembershipConfig, provision_user_membership};

pub(crate) use super::xmpp_channels::{XmppChannelRecord, get_xmpp_channel, list_xmpp_channels};

#[cfg(test)]
fn db_string(
    row: &crate::db::actor::RowValues,
    index: usize,
    name: &str,
) -> Result<String, String> {
    row_value(row, index)
        .and_then(ValueExt::as_string)
        .map_err(|e| format!("Failed to get {name}: {e}"))
}

#[cfg(test)]
fn db_optional_string(
    row: &crate::db::actor::RowValues,
    index: usize,
    name: &str,
) -> Result<Option<String>, String> {
    row_value(row, index)
        .and_then(ValueExt::as_optional_string)
        .map_err(|e| format!("Failed to get {name}: {e}"))
}

/// XMPP application state that bridges to waddle-server services.
///
/// This struct implements `waddle_xmpp::AppState` by delegating to:
/// - `SessionManager` for session validation
/// - `PermissionActor` for permission checks
/// - `NativeUserStore` for XEP-0077 registration and SCRAM authentication
/// - `VCardStore` for XEP-0054 vcard-temp storage
/// - `Database` for upload slot storage (XEP-0363)
#[cfg(test)]
pub struct XmppAppState {
    /// The XMPP server domain (e.g., "waddle.social")
    domain: String,
    /// Session manager for validating XMPP authentication tokens
    session_manager: SessionManager,
    /// Permission actor for authorization checks
    permission_actor: ActorRef<PermissionActor>,
    /// Native user store for XEP-0077 registration and SCRAM authentication
    native_user_store: NativeUserStore,
    /// vCard store for XEP-0054 vcard-temp
    vcard_store: VCardStore,
    /// Global database actor for runtime repository access.
    global_db_actor: ActorRef<DbActor>,
    /// Database actor for canonical space data.
    space_db_actor: Option<ActorRef<DbActor>>,
    /// Shared Waddle inbox projection storage.
    inbox_storage: Option<Arc<dyn InboxStorage>>,
}

#[cfg(test)]
impl XmppAppState {
    /// Create a new XMPP application state.
    ///
    /// # Arguments
    ///
    /// * `domain` - The XMPP server domain (e.g., "waddle.social")
    /// * `db` - The global database for session and permission storage
    /// * `encryption_key` - Optional encryption key for session token encryption
    pub fn new(
        domain: String,
        db: Arc<Database>,
        db_actor: ActorRef<DbActor>,
        permission_actor: ActorRef<PermissionActor>,
        encryption_key: Option<&[u8]>,
    ) -> Self {
        let session_manager = SessionManager::new(db_actor.clone(), encryption_key);
        let native_user_store = NativeUserStore::new(db_actor.clone());
        let vcard_store = VCardStore::new(Arc::clone(&db));

        Self {
            domain,
            session_manager,
            permission_actor,
            native_user_store,
            vcard_store,
            global_db_actor: db_actor,
            space_db_actor: None,
            inbox_storage: None,
        }
    }

    /// Parse a resource string into an Object.
    ///
    /// Resource format: "space:{id}" or "channel:{id}"
    fn parse_resource(resource: &str) -> Result<Object, XmppError> {
        super::xmpp_permission_state::parse_resource(resource)
    }

    /// Parse a subject string into a Subject.
    ///
    /// Subject format: "user:{user_id}" or "space:{id}#member"
    fn parse_subject(subject: &str) -> Result<Subject, XmppError> {
        super::xmpp_permission_state::parse_subject(subject)
    }

    async fn global_database(&self) -> Result<Database, XmppError> {
        self.global_db_actor
            .ask(GetDatabase)
            .await
            .map_err(|e| XmppError::internal(format!("Failed to access global database: {}", e)))
    }
}

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
        debug!(jid = %jid, "Validating XMPP session");

        // Convert JID to localpart for verification
        let expected_localpart = jid_to_localpart(&jid.to_string()).map_err(|e| {
            warn!(jid = %jid, error = %e, "Failed to extract localpart from JID");
            XmppError::auth_failed(format!("Invalid JID format: {}", e))
        })?;

        // Validate the session token (which is the session ID)
        let session = self
            .session_manager
            .validate_session(token)
            .await
            .map_err(|e| {
                warn!(token_prefix = %&token[..token.len().min(8)], error = %e, "Session validation failed");
                match e {
                    crate::auth::AuthError::SessionNotFound(_) => XmppError::SessionNotFound,
                    crate::auth::AuthError::SessionExpired => XmppError::SessionNotFound,
                    _ => XmppError::auth_failed(format!("Session validation failed: {}", e)),
                }
            })?;

        // Verify the localpart matches the immutable session localpart.
        if session.xmpp_localpart != expected_localpart {
            warn!(
                expected_localpart = %expected_localpart,
                session_localpart = %session.xmpp_localpart,
                "Localpart mismatch between JID and session"
            );
            return Err(XmppError::auth_failed("JID does not match session"));
        }

        // Convert to XMPP session
        let bare_jid = jid.to_bare();

        // Calculate expires_at - use session expiry or default to 24 hours from now
        let expires_at = session
            .expires_at
            .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::hours(24));

        Ok(XmppSession {
            user_id: session.user_id,
            jid: bare_jid,
            created_at: session.created_at,
            expires_at,
        })
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
        debug!(token_prefix = %&token[..token.len().min(8)], "Validating XMPP session token (OAUTHBEARER)");

        // Validate the session token (which is the session ID)
        let session = self
            .session_manager
            .validate_session(token)
            .await
            .map_err(|e| {
                warn!(token_prefix = %&token[..token.len().min(8)], error = %e, "Session validation failed");
                match e {
                    crate::auth::AuthError::SessionNotFound(_) => XmppError::SessionNotFound,
                    crate::auth::AuthError::SessionExpired => XmppError::SessionNotFound,
                    _ => XmppError::auth_failed(format!("Session validation failed: {}", e)),
                }
            })?;

        // Convert immutable localpart to JID
        let jid_str = localpart_to_jid(&session.xmpp_localpart, &self.domain).map_err(|e| {
            warn!(localpart = %session.xmpp_localpart, error = %e, "Failed to convert localpart to JID");
            XmppError::auth_failed(format!("Invalid localpart format: {}", e))
        })?;

        let bare_jid: jid::BareJid = jid_str.parse().map_err(|e| {
            warn!(jid = %jid_str, error = ?e, "Failed to parse generated JID");
            XmppError::auth_failed(format!("Invalid JID: {:?}", e))
        })?;

        // Calculate expires_at - use session expiry or default to 24 hours from now
        let expires_at = session
            .expires_at
            .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::hours(24));

        debug!(jid = %bare_jid, user_id = %session.user_id, "OAUTHBEARER session validated");

        Ok(XmppSession {
            user_id: session.user_id,
            jid: bare_jid,
            created_at: session.created_at,
            expires_at,
        })
    }

    /// Get the XMPP server domain.
    fn domain(&self) -> &str {
        &self.domain
    }

    /// Get the OAuth discovery URL for XMPP OAUTHBEARER (XEP-0493).
    ///
    /// Returns the RFC 8414 OAuth authorization server metadata endpoint URL.
    fn oauth_discovery_url(&self) -> String {
        // Construct the discovery URL based on the domain
        // In production, this should be configurable via environment variable
        let base_url =
            std::env::var("WADDLE_BASE_URL").unwrap_or_else(|_| format!("https://{}", self.domain));
        format!(
            "{}/.well-known/oauth-authorization-server",
            base_url.trim_end_matches('/')
        )
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
    ) -> Result<Vec<UserDirectoryEntry>, XmppError> {
        let pattern = format!("%{}%", query.trim());
        let rows = self
            .global_db_actor
            .ask(DbQuery {
                sql: r#"
                    SELECT username, xmpp_localpart, display_name, avatar_url
                    FROM users
                    WHERE username LIKE ? OR xmpp_localpart LIKE ? OR display_name LIKE ?
                    ORDER BY username ASC
                    LIMIT ?
                "#
                .to_string(),
                params: vec![
                    pattern.as_str().into(),
                    pattern.as_str().into(),
                    pattern.as_str().into(),
                    (limit as i64).into(),
                ],
            })
            .await
            .map_err(|e| XmppError::internal(format!("Failed to search users: {}", e)))?;

        rows.iter()
            .map(|row| {
                let username = db_string(row, 0, "username").map_err(|e| {
                    XmppError::internal(format!("Failed to decode username: {}", e))
                })?;
                let localpart = db_string(row, 1, "xmpp_localpart").map_err(|e| {
                    XmppError::internal(format!("Failed to decode xmpp_localpart: {}", e))
                })?;
                let display_name = db_optional_string(row, 2, "display_name").map_err(|e| {
                    XmppError::internal(format!("Failed to decode display_name: {}", e))
                })?;
                let avatar_url = db_optional_string(row, 3, "avatar_url").map_err(|e| {
                    XmppError::internal(format!("Failed to decode avatar_url: {}", e))
                })?;
                let jid = localpart_to_jid(&localpart, &self.domain)
                    .map_err(|e| XmppError::internal(format!("Failed to build user JID: {}", e)))?
                    .parse()
                    .map_err(|e| XmppError::internal(format!("Failed to parse user JID: {}", e)))?;
                Ok(UserDirectoryEntry {
                    jid,
                    username,
                    display_name,
                    avatar_url,
                })
            })
            .collect()
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
        debug!(
            username = username,
            domain = %self.domain,
            "Looking up SCRAM credentials for native user"
        );

        match self
            .native_user_store
            .get_scram_credentials(username, &self.domain)
            .await
        {
            Ok(Some(creds)) => {
                debug!(username = username, "Found SCRAM credentials");
                Ok(Some(creds))
            }
            Ok(None) => {
                debug!(username = username, "No SCRAM credentials found");
                Ok(None)
            }
            Err(e) => {
                warn!(username = username, error = %e, "Failed to lookup SCRAM credentials");
                Err(XmppError::internal(format!("Database error: {}", e)))
            }
        }
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
        debug!(
            username = username,
            domain = %self.domain,
            has_email = email.is_some(),
            "Registering native user via XEP-0077"
        );

        let request = RegisterRequest {
            username: username.to_string(),
            domain: self.domain.clone(),
            password: password.to_string(),
            email: email.map(|s| s.to_string()),
        };

        match self.native_user_store.register(request).await {
            Ok(user_id) => {
                let subject_user_id = format!("{}@{}", username, self.domain);
                provision_user_membership(
                    &self.permission_actor,
                    &BootstrapMembershipConfig::from_env(),
                    &subject_user_id,
                    username,
                )
                .await
                .map_err(|err| {
                    XmppError::internal(format!("Failed to provision account membership: {err}"))
                })?;
                debug!(
                    username = username,
                    user_id = user_id,
                    "Native user registered successfully"
                );
                Ok(())
            }
            Err(crate::auth::AuthError::UserAlreadyExists(_)) => {
                warn!(
                    username = username,
                    "Registration failed: user already exists"
                );
                Err(XmppError::conflict(Some(format!(
                    "User '{}' already exists",
                    username
                ))))
            }
            Err(crate::auth::AuthError::InvalidUsername(msg)) => {
                warn!(username = username, error = %msg, "Registration failed: invalid username");
                Err(XmppError::not_acceptable(Some(msg)))
            }
            Err(e) => {
                warn!(username = username, error = %e, "Registration failed");
                Err(XmppError::internal(format!("Registration failed: {}", e)))
            }
        }
    }

    /// Check if a native user exists.
    async fn native_user_exists(&self, username: &str) -> Result<bool, XmppError> {
        debug!(
            username = username,
            domain = %self.domain,
            "Checking if native user exists"
        );

        match self
            .native_user_store
            .user_exists(username, &self.domain)
            .await
        {
            Ok(exists) => Ok(exists),
            Err(e) => {
                warn!(username = username, error = %e, "Failed to check user existence");
                Err(XmppError::internal(format!("Database error: {}", e)))
            }
        }
    }

    /// Get the vCard for a user (XEP-0054).
    async fn get_vcard(&self, jid: &jid::BareJid) -> Result<Option<String>, XmppError> {
        debug!(jid = %jid, "Getting vCard");

        match self.vcard_store.get(jid).await {
            Ok(vcard) => Ok(vcard),
            Err(e) => {
                warn!(jid = %jid, error = %e, "Failed to get vCard");
                Err(XmppError::internal(format!("Database error: {}", e)))
            }
        }
    }

    /// Store/update the vCard for a user (XEP-0054).
    async fn set_vcard(&self, jid: &jid::BareJid, vcard_xml: &str) -> Result<(), XmppError> {
        debug!(jid = %jid, "Setting vCard");

        match self.vcard_store.set(jid, vcard_xml).await {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(jid = %jid, error = %e, "Failed to set vCard");
                Err(XmppError::internal(format!("Database error: {}", e)))
            }
        }
    }

    /// Look up the externally-hosted avatar URL for a JID (XEP-0084 `url=`).
    ///
    /// Reads `users.avatar_url` keyed on `xmpp_localpart`. The URL is the one
    /// captured during OIDC login (e.g. a GitHub avatar). Missing or empty
    /// values return `Ok(None)`.
    async fn get_user_avatar_url(&self, jid: &jid::BareJid) -> Result<Option<String>, XmppError> {
        let Some(localpart) = jid.node().map(|n| n.to_string()) else {
            return Ok(None);
        };

        let row = self
            .global_db_actor
            .ask(DbQueryOne {
                sql: "SELECT avatar_url FROM users WHERE xmpp_localpart = ? LIMIT 1".to_string(),
                params: vec![crate::db::Value::from(localpart.clone())],
            })
            .await
            .map_err(|e| {
                warn!(jid = %jid, error = %e, "avatar_url query failed");
                XmppError::internal(format!("Database actor error: {}", e))
            })?;

        let Some(row) = row else {
            return Ok(None);
        };

        let url = row_value(&row, 0)
            .and_then(|value| value.as_optional_string())
            .map_err(|e| {
                warn!(jid = %jid, error = %e, "avatar_url column decode failed");
                XmppError::internal(format!("Database error: {}", e))
            })?;

        Ok(url.filter(|s| !s.is_empty()))
    }

    /// Create an upload slot for XEP-0363 HTTP File Upload.
    async fn create_upload_slot(
        &self,
        requester_jid: &jid::BareJid,
        filename: &str,
        size: u64,
        content_type: Option<&str>,
    ) -> Result<waddle_xmpp::UploadSlotInfo, XmppError> {
        use waddle_xmpp::xep::xep0363::{effective_content_type, sanitize_filename};

        debug!(
            jid = %requester_jid,
            filename = %filename,
            size = size,
            content_type = ?content_type,
            "Creating upload slot"
        );

        // Check file size limit
        let max_size = self.max_upload_size();
        if size > max_size {
            warn!(
                jid = %requester_jid,
                size = size,
                max_size = max_size,
                "File too large for upload"
            );
            return Err(XmppError::not_acceptable(Some(format!(
                "File too large. Maximum size is {} bytes.",
                max_size
            ))));
        }

        // Sanitize the filename
        let safe_filename = sanitize_filename(filename);
        let effective_type = effective_content_type(content_type).to_string();

        // Generate a unique slot ID
        let slot_id = uuid::Uuid::new_v4().to_string();

        // Calculate expiration (15 minutes from now)
        let expires_at = chrono::Utc::now() + chrono::Duration::minutes(15);

        // Get the base URL for uploads
        let base_url =
            std::env::var("WADDLE_BASE_URL").unwrap_or_else(|_| format!("https://{}", self.domain));
        let base_url = base_url.trim_end_matches('/');

        // Build the PUT and GET URLs
        let put_url = format!("{}/api/upload/{}", base_url, slot_id);
        let get_url = format!("{}/api/files/{}/{}", base_url, slot_id, safe_filename);

        // Store the slot in the database
        self.global_db_actor
            .ask(DbExecute {
                sql: "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at) VALUES (?, ?, ?, ?, ?, 'pending', ?)".to_string(),
                params: vec![
                    crate::db::Value::from(slot_id.clone()),
                    crate::db::Value::from(requester_jid.to_string()),
                    crate::db::Value::from(safe_filename.clone()),
                    crate::db::Value::from(size as i64),
                    crate::db::Value::from(effective_type.clone()),
                    crate::db::Value::from(expires_at.to_rfc3339()),
                ],
            })
            .await
            .map_err(|e| {
                warn!(error = %e, "Failed to create upload slot in database");
                XmppError::internal(format!("Database actor error: {}", e))
            })?;

        debug!(
            slot_id = %slot_id,
            put_url = %put_url,
            get_url = %get_url,
            "Created upload slot"
        );

        Ok(waddle_xmpp::UploadSlotInfo {
            put_url,
            get_url,
            put_headers: vec![("Content-Type".to_string(), effective_type)],
        })
    }

    /// Get the maximum allowed file upload size in bytes.
    fn max_upload_size(&self) -> u64 {
        // Check environment variable, default to 10 MB
        std::env::var("WADDLE_MAX_UPLOAD_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10 * 1024 * 1024)
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
mod tests {
    use super::*;
    use crate::db::MigrationRunner;
    use crate::permissions::{ObjectType, PermissionActor};
    use waddle_xmpp::AppState;

    async fn create_test_db() -> (Arc<Database>, ActorRef<DbActor>) {
        let db = Database::in_memory("test-xmpp-state")
            .await
            .expect("Failed to create test database");
        let db = Arc::new(db);

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(&db).await.expect("Failed to run migrations");

        let actor = kameo::spawn(DbActor::new((*db).clone()));
        (db, actor)
    }

    #[tokio::test]
    async fn test_xmpp_state_creation() {
        let (db, actor) = create_test_db().await;
        let state = XmppAppState::new(
            "waddle.social".to_string(),
            Arc::clone(&db),
            actor,
            kameo::spawn(PermissionActor::new_for_tests(db)),
            None,
        );

        assert_eq!(state.domain(), "waddle.social");
    }

    #[tokio::test]
    async fn test_parse_resource() {
        let obj = XmppAppState::parse_resource("space:penguin-club").expect("Failed to parse");
        assert_eq!(obj.object_type, ObjectType::Space);
        assert_eq!(obj.id, "penguin-club");

        let obj = XmppAppState::parse_resource("channel:general").expect("Failed to parse");
        assert_eq!(obj.object_type, ObjectType::Channel);
        assert_eq!(obj.id, "general");
    }

    #[tokio::test]
    async fn test_parse_subject() {
        let subj = XmppAppState::parse_subject("user:user-abc123").expect("Failed to parse");
        assert_eq!(subj.id, "user-abc123");
        assert!(subj.relation.is_none());
    }

    #[tokio::test]
    async fn test_parse_invalid_resource() {
        let result = XmppAppState::parse_resource("invalid");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_parse_invalid_subject() {
        let result = XmppAppState::parse_subject("invalid");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_private_xml_roundtrip_uses_actor_boundary() {
        let (db, actor) = create_test_db().await;
        let state = XmppAppState::new(
            "waddle.social".to_string(),
            Arc::clone(&db),
            actor,
            kameo::spawn(PermissionActor::new_for_tests(db)),
            None,
        );
        let jid: jid::BareJid = "alice@waddle.social".parse().expect("valid bare jid");

        state
            .set_private_xml(
                &jid,
                "urn:xmpp:test",
                "<prefs><theme>aether</theme></prefs>",
            )
            .await
            .expect("set private xml");

        let stored = state
            .get_private_xml(&jid, "urn:xmpp:test")
            .await
            .expect("get private xml");
        assert_eq!(
            stored.as_deref(),
            Some("<prefs><theme>aether</theme></prefs>")
        );
    }
}
