//! XMPP AppState implementation bridging to waddle-server services.
//!
//! This module implements the `waddle_xmpp::AppState` trait by delegating to
//! the existing auth, session, and permission services in waddle-server.

use std::sync::Arc;

use tracing::{debug, warn};
use waddle_xmpp::{Session as XmppSession, XmppError};

use crate::auth::{
    jid_to_localpart, localpart_to_jid, NativeUserStore, RegisterRequest, SessionManager,
};
use crate::db::{Database, DatabasePool, MigrationRunner};
use crate::permissions::{Object, PermissionService, Subject};
use crate::server::routes::channels::list_channels_from_db as list_channels_for_waddle_from_db;
use crate::server::routes::waddles::list_user_waddles as list_user_waddles_from_db;
use crate::vcard::VCardStore;

/// XMPP application state that bridges to waddle-server services.
///
/// This struct implements `waddle_xmpp::AppState` by delegating to:
/// - `SessionManager` for session validation
/// - `PermissionService` for permission checks
/// - `NativeUserStore` for XEP-0077 registration and SCRAM authentication
/// - `VCardStore` for XEP-0054 vcard-temp storage
/// - `Database` for upload slot storage (XEP-0363)
pub struct XmppAppState {
    /// The XMPP server domain (e.g., "waddle.social")
    domain: String,
    /// Session manager for validating XMPP authentication tokens
    session_manager: SessionManager,
    /// Permission service for authorization checks
    permission_service: PermissionService,
    /// Native user store for XEP-0077 registration and SCRAM authentication
    native_user_store: NativeUserStore,
    /// vCard store for XEP-0054 vcard-temp
    vcard_store: VCardStore,
    /// Database for upload slots and other direct DB operations
    db: Arc<Database>,
    /// Database pool for per-waddle database access (auto-join enumeration)
    db_pool: Option<Arc<DatabasePool>>,
}

impl XmppAppState {
    /// Create a new XMPP application state.
    ///
    /// # Arguments
    ///
    /// * `domain` - The XMPP server domain (e.g., "waddle.social")
    /// * `db` - The global database for session and permission storage
    /// * `encryption_key` - Optional encryption key for session token encryption
    pub fn new(domain: String, db: Arc<Database>, encryption_key: Option<&[u8]>) -> Self {
        let session_manager = SessionManager::new(Arc::clone(&db), encryption_key);
        let permission_service = PermissionService::new(Arc::clone(&db));
        let native_user_store = NativeUserStore::new(Arc::clone(&db));
        let vcard_store = VCardStore::new(Arc::clone(&db));

        Self {
            domain,
            session_manager,
            permission_service,
            native_user_store,
            vcard_store,
            db,
            db_pool: None,
        }
    }

    /// Set the database pool for per-waddle database access.
    ///
    /// This enables auto-join channel enumeration by providing access
    /// to per-waddle SQLite databases.
    pub fn with_db_pool(mut self, db_pool: Arc<DatabasePool>) -> Self {
        self.db_pool = Some(db_pool);
        self
    }

    /// Parse a resource string into an Object.
    ///
    /// Resource format: "waddle:{id}" or "channel:{id}"
    fn parse_resource(resource: &str) -> Result<Object, XmppError> {
        Object::parse(resource).map_err(|e| {
            XmppError::internal(format!("Invalid resource format '{}': {}", resource, e))
        })
    }

    /// Parse a subject string into a Subject.
    ///
    /// Subject format: "user:{user_id}" or "waddle:{id}#member"
    fn parse_subject(subject: &str) -> Result<Subject, XmppError> {
        Subject::parse(subject).map_err(|e| {
            XmppError::internal(format!("Invalid subject format '{}': {}", subject, e))
        })
    }
}

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
    /// Resource format: "waddle:{id}" or "channel:{id}"
    /// Subject format: "user:{user_id}"
    async fn check_permission(
        &self,
        resource: &str,
        action: &str,
        subject: &str,
    ) -> Result<bool, XmppError> {
        debug!(
            resource = resource,
            action = action,
            subject = subject,
            "Checking XMPP permission"
        );

        let object = Self::parse_resource(resource)?;
        let subject = Self::parse_subject(subject)?;

        let response = self
            .permission_service
            .check(&subject, action, &object)
            .await
            .map_err(|e| {
                warn!(
                    resource = resource,
                    action = action,
                    error = %e,
                    "Permission check failed"
                );
                XmppError::internal(format!("Permission check failed: {}", e))
            })?;

        debug!(
            resource = resource,
            action = action,
            allowed = response.allowed,
            "Permission check result"
        );

        Ok(response.allowed)
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
        debug!(
            resource = resource,
            subject = subject,
            "Listing relations for subject on resource"
        );

        let object = Self::parse_resource(resource)?;
        let subject = Self::parse_subject(subject)?;

        let relations = self
            .permission_service
            .list_relations(&subject, &object)
            .await
            .map_err(|e| {
                warn!(
                    resource = resource,
                    error = %e,
                    "Failed to list relations"
                );
                XmppError::internal(format!("Failed to list relations: {}", e))
            })?;

        debug!(
            resource = resource,
            relations = ?relations,
            "Listed relations"
        );

        Ok(relations)
    }

    /// List all subjects with a specific relation on an object.
    ///
    /// Used for MUC affiliation list queries (XEP-0045).
    async fn list_subjects(
        &self,
        resource: &str,
        relation: &str,
    ) -> Result<Vec<String>, XmppError> {
        debug!(
            resource = resource,
            relation = relation,
            "Listing subjects with relation on resource"
        );

        let object = Self::parse_resource(resource)?;

        let subjects = self
            .permission_service
            .tuple_store
            .list_subjects(&object, relation)
            .await
            .map_err(|e| {
                warn!(
                    resource = resource,
                    relation = relation,
                    error = %e,
                    "Failed to list subjects"
                );
                XmppError::internal(format!("Failed to list subjects: {}", e))
            })?;

        // Convert Subject objects to string format
        let subject_strings: Vec<String> = subjects.iter().map(|s| s.to_string()).collect();

        debug!(
            resource = resource,
            relation = relation,
            count = subject_strings.len(),
            "Listed subjects"
        );

        Ok(subject_strings)
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
        // Use persistent connection for in-memory databases
        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(
                "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at) VALUES (?, ?, ?, ?, ?, 'pending', ?)",
                libsql::params![
                    slot_id.clone(),
                    requester_jid.to_string(),
                    safe_filename.clone(),
                    size as i64,
                    effective_type.clone(),
                    expires_at.to_rfc3339(),
                ],
            ).await.map_err(|e| {
                warn!(error = %e, "Failed to create upload slot in database");
                XmppError::internal(format!("Database error: {}", e))
            })?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                warn!(error = %e, "Failed to connect to database for upload slot");
                XmppError::internal(format!("Database error: {}", e))
            })?;
            conn.execute(
                "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at) VALUES (?, ?, ?, ?, ?, 'pending', ?)",
                libsql::params![
                    slot_id.clone(),
                    requester_jid.to_string(),
                    safe_filename.clone(),
                    size as i64,
                    effective_type.clone(),
                    expires_at.to_rfc3339(),
                ],
            ).await.map_err(|e| {
                warn!(error = %e, "Failed to create upload slot in database");
                XmppError::internal(format!("Database error: {}", e))
            })?;
        }

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
        use crate::db::roster::DatabaseRosterStorage;

        debug!(jid = %user_jid, "Getting roster");

        // Clone the Database to create a DatabaseRosterStorage
        let storage = DatabaseRosterStorage::new((*self.db).clone());

        let rows = storage.get_roster(user_jid).await.map_err(|e| {
            warn!(jid = %user_jid, error = %e, "Failed to get roster");
            XmppError::internal(format!("Database error: {}", e))
        })?;

        // Convert RosterItemRow to RosterItem
        let items: Result<Vec<_>, _> = rows
            .into_iter()
            .map(|row| row_to_roster_item(&row))
            .collect();

        items.map_err(|e| {
            warn!(jid = %user_jid, error = %e, "Failed to convert roster items");
            XmppError::internal(format!("Roster conversion error: {}", e))
        })
    }

    /// Get a single roster item by JID.
    async fn get_roster_item(
        &self,
        user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
    ) -> Result<Option<waddle_xmpp::roster::RosterItem>, XmppError> {
        use crate::db::roster::DatabaseRosterStorage;

        debug!(user = %user_jid, contact = %contact_jid, "Getting roster item");

        let storage = DatabaseRosterStorage::new((*self.db).clone());

        let row = storage.get_roster_item(user_jid, contact_jid).await.map_err(|e| {
            warn!(user = %user_jid, contact = %contact_jid, error = %e, "Failed to get roster item");
            XmppError::internal(format!("Database error: {}", e))
        })?;

        match row {
            Some(r) => row_to_roster_item(&r).map(Some).map_err(|e| {
                warn!(user = %user_jid, contact = %contact_jid, error = %e, "Failed to convert roster item");
                XmppError::internal(format!("Roster conversion error: {}", e))
            }),
            None => Ok(None),
        }
    }

    /// Add or update a roster item.
    async fn set_roster_item(
        &self,
        user_jid: &jid::BareJid,
        item: &waddle_xmpp::roster::RosterItem,
    ) -> Result<waddle_xmpp::roster::RosterSetResult, XmppError> {
        use crate::db::roster::DatabaseRosterStorage;

        debug!(user = %user_jid, contact = %item.jid, "Setting roster item");

        let storage = DatabaseRosterStorage::new((*self.db).clone());

        let row = roster_item_to_row(item);
        let is_new = storage.set_roster_item(user_jid, &row).await.map_err(|e| {
            warn!(user = %user_jid, contact = %item.jid, error = %e, "Failed to set roster item");
            XmppError::internal(format!("Database error: {}", e))
        })?;

        if is_new {
            Ok(waddle_xmpp::roster::RosterSetResult::Added(item.clone()))
        } else {
            Ok(waddle_xmpp::roster::RosterSetResult::Updated(item.clone()))
        }
    }

    /// Remove a roster item.
    async fn remove_roster_item(
        &self,
        user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
    ) -> Result<bool, XmppError> {
        use crate::db::roster::DatabaseRosterStorage;

        debug!(user = %user_jid, contact = %contact_jid, "Removing roster item");

        let storage = DatabaseRosterStorage::new((*self.db).clone());

        storage.remove_roster_item(user_jid, contact_jid).await.map_err(|e| {
            warn!(user = %user_jid, contact = %contact_jid, error = %e, "Failed to remove roster item");
            XmppError::internal(format!("Database error: {}", e))
        })
    }

    /// Get the current roster version for a user.
    async fn get_roster_version(
        &self,
        user_jid: &jid::BareJid,
    ) -> Result<Option<String>, XmppError> {
        use crate::db::roster::DatabaseRosterStorage;

        debug!(jid = %user_jid, "Getting roster version");

        let storage = DatabaseRosterStorage::new((*self.db).clone());

        storage.get_roster_version(user_jid).await.map_err(|e| {
            warn!(jid = %user_jid, error = %e, "Failed to get roster version");
            XmppError::internal(format!("Database error: {}", e))
        })
    }

    /// Update the subscription state for a roster item.
    async fn update_roster_subscription(
        &self,
        user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
        subscription: waddle_xmpp::roster::Subscription,
        ask: Option<waddle_xmpp::roster::AskType>,
    ) -> Result<waddle_xmpp::roster::RosterItem, XmppError> {
        use crate::db::roster::DatabaseRosterStorage;

        debug!(
            user = %user_jid,
            contact = %contact_jid,
            subscription = %subscription,
            ask = ?ask,
            "Updating roster subscription"
        );

        let storage = DatabaseRosterStorage::new((*self.db).clone());

        let subscription_str = subscription.as_str();
        let ask_str = ask.map(|a| a.as_str());

        let row = storage
            .update_subscription(user_jid, contact_jid, subscription_str, ask_str)
            .await
            .map_err(|e| {
                warn!(
                    user = %user_jid,
                    contact = %contact_jid,
                    error = %e,
                    "Failed to update roster subscription"
                );
                XmppError::internal(format!("Database error: {}", e))
            })?;

        row_to_roster_item(&row).map_err(|e| {
            warn!(user = %user_jid, contact = %contact_jid, error = %e, "Failed to convert roster item");
            XmppError::internal(format!("Roster conversion error: {}", e))
        })
    }

    /// Get all roster items where the user should send presence updates.
    async fn get_presence_subscribers(
        &self,
        user_jid: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, XmppError> {
        use crate::db::roster::DatabaseRosterStorage;

        debug!(jid = %user_jid, "Getting presence subscribers");

        let storage = DatabaseRosterStorage::new((*self.db).clone());

        let jid_strings = storage
            .get_presence_subscribers(user_jid)
            .await
            .map_err(|e| {
                warn!(jid = %user_jid, error = %e, "Failed to get presence subscribers");
                XmppError::internal(format!("Database error: {}", e))
            })?;

        // Parse JID strings into BareJids
        let jids: Result<Vec<_>, _> = jid_strings
            .iter()
            .map(|s| s.parse::<jid::BareJid>())
            .collect();

        jids.map_err(|e| {
            warn!(jid = %user_jid, error = ?e, "Failed to parse presence subscriber JIDs");
            XmppError::internal(format!("JID parse error: {:?}", e))
        })
    }

    /// Get all roster items where the user receives presence updates.
    async fn get_presence_subscriptions(
        &self,
        user_jid: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, XmppError> {
        use crate::db::roster::DatabaseRosterStorage;

        debug!(jid = %user_jid, "Getting presence subscriptions");

        let storage = DatabaseRosterStorage::new((*self.db).clone());

        let jid_strings = storage
            .get_presence_subscriptions(user_jid)
            .await
            .map_err(|e| {
                warn!(jid = %user_jid, error = %e, "Failed to get presence subscriptions");
                XmppError::internal(format!("Database error: {}", e))
            })?;

        // Parse JID strings into BareJids
        let jids: Result<Vec<_>, _> = jid_strings
            .iter()
            .map(|s| s.parse::<jid::BareJid>())
            .collect();

        jids.map_err(|e| {
            warn!(jid = %user_jid, error = ?e, "Failed to parse presence subscription JIDs");
            XmppError::internal(format!("JID parse error: {:?}", e))
        })
    }

    // =========================================================================
    // XEP-0191 Blocking Command Methods
    // =========================================================================

    /// Get all blocked JIDs for a user.
    async fn get_blocklist(&self, user_jid: &jid::BareJid) -> Result<Vec<String>, XmppError> {
        use crate::db::blocking::DatabaseBlockingStorage;

        debug!(jid = %user_jid, "Getting blocklist");

        let storage = DatabaseBlockingStorage::new((*self.db).clone());

        storage.get_blocklist(user_jid).await.map_err(|e| {
            warn!(jid = %user_jid, error = %e, "Failed to get blocklist");
            XmppError::internal(format!("Database error: {}", e))
        })
    }

    /// Check if a JID is blocked by a user.
    async fn is_blocked(
        &self,
        user_jid: &jid::BareJid,
        blocked_jid: &jid::BareJid,
    ) -> Result<bool, XmppError> {
        use crate::db::blocking::DatabaseBlockingStorage;

        debug!(user = %user_jid, blocked = %blocked_jid, "Checking if JID is blocked");

        let storage = DatabaseBlockingStorage::new((*self.db).clone());

        storage.is_blocked(user_jid, blocked_jid).await.map_err(|e| {
            warn!(user = %user_jid, blocked = %blocked_jid, error = %e, "Failed to check if blocked");
            XmppError::internal(format!("Database error: {}", e))
        })
    }

    /// Add JIDs to a user's blocklist.
    async fn add_blocks(
        &self,
        user_jid: &jid::BareJid,
        blocked_jids: &[String],
    ) -> Result<usize, XmppError> {
        use crate::db::blocking::DatabaseBlockingStorage;

        debug!(jid = %user_jid, count = blocked_jids.len(), "Adding blocks");

        let storage = DatabaseBlockingStorage::new((*self.db).clone());

        storage
            .add_blocks(user_jid, blocked_jids)
            .await
            .map_err(|e| {
                warn!(jid = %user_jid, error = %e, "Failed to add blocks");
                XmppError::internal(format!("Database error: {}", e))
            })
    }

    /// Remove JIDs from a user's blocklist.
    async fn remove_blocks(
        &self,
        user_jid: &jid::BareJid,
        blocked_jids: &[String],
    ) -> Result<usize, XmppError> {
        use crate::db::blocking::DatabaseBlockingStorage;

        debug!(jid = %user_jid, count = blocked_jids.len(), "Removing blocks");

        let storage = DatabaseBlockingStorage::new((*self.db).clone());

        storage
            .remove_blocks(user_jid, blocked_jids)
            .await
            .map_err(|e| {
                warn!(jid = %user_jid, error = %e, "Failed to remove blocks");
                XmppError::internal(format!("Database error: {}", e))
            })
    }

    /// Remove all JIDs from a user's blocklist.
    async fn remove_all_blocks(&self, user_jid: &jid::BareJid) -> Result<usize, XmppError> {
        use crate::db::blocking::DatabaseBlockingStorage;

        debug!(jid = %user_jid, "Removing all blocks");

        let storage = DatabaseBlockingStorage::new((*self.db).clone());

        storage.remove_all_blocks(user_jid).await.map_err(|e| {
            warn!(jid = %user_jid, error = %e, "Failed to remove all blocks");
            XmppError::internal(format!("Database error: {}", e))
        })
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
        debug!(jid = %jid, namespace = %namespace, "Getting private XML");

        let query = "SELECT xml_content FROM private_xml_storage WHERE jid = ? AND namespace = ?";
        let params = libsql::params![jid.to_string(), namespace.to_string()];

        let result = if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            let mut rows = conn.query(query, params).await.map_err(|e| {
                warn!(jid = %jid, namespace = %namespace, error = %e, "Failed to get private XML");
                XmppError::internal(format!("Database error: {}", e))
            })?;
            match rows
                .next()
                .await
                .map_err(|e| XmppError::internal(format!("Database error: {}", e)))?
            {
                Some(row) => {
                    let xml: String = row
                        .get(0)
                        .map_err(|e| XmppError::internal(format!("Database error: {}", e)))?;
                    Some(xml)
                }
                None => None,
            }
        } else {
            let conn = self
                .db
                .connect()
                .map_err(|e| XmppError::internal(format!("Database error: {}", e)))?;
            let mut rows = conn.query(query, params).await.map_err(|e| {
                warn!(jid = %jid, namespace = %namespace, error = %e, "Failed to get private XML");
                XmppError::internal(format!("Database error: {}", e))
            })?;
            match rows
                .next()
                .await
                .map_err(|e| XmppError::internal(format!("Database error: {}", e)))?
            {
                Some(row) => {
                    let xml: String = row
                        .get(0)
                        .map_err(|e| XmppError::internal(format!("Database error: {}", e)))?;
                    Some(xml)
                }
                None => None,
            }
        };

        Ok(result)
    }

    /// Store/update private XML data for a user by namespace.
    async fn set_private_xml(
        &self,
        jid: &jid::BareJid,
        namespace: &str,
        xml_content: &str,
    ) -> Result<(), XmppError> {
        debug!(jid = %jid, namespace = %namespace, "Setting private XML");

        let query = "INSERT OR REPLACE INTO private_xml_storage (jid, namespace, xml_content, updated_at) VALUES (?, ?, ?, datetime('now'))";
        let params = libsql::params![
            jid.to_string(),
            namespace.to_string(),
            xml_content.to_string()
        ];

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(query, params).await.map_err(|e| {
                warn!(jid = %jid, namespace = %namespace, error = %e, "Failed to set private XML");
                XmppError::internal(format!("Database error: {}", e))
            })?;
        } else {
            let conn = self
                .db
                .connect()
                .map_err(|e| XmppError::internal(format!("Database error: {}", e)))?;
            conn.execute(query, params).await.map_err(|e| {
                warn!(jid = %jid, namespace = %namespace, error = %e, "Failed to set private XML");
                XmppError::internal(format!("Database error: {}", e))
            })?;
        }

        Ok(())
    }

    // =========================================================================
    // Auto-Join: Waddle & Channel Enumeration
    // =========================================================================

    /// List all waddles a user belongs to.
    async fn list_user_waddles(
        &self,
        user_id: &str,
    ) -> Result<Vec<waddle_xmpp::WaddleInfo>, XmppError> {
        debug!(user_id = %user_id, "Listing user waddles for auto-join");

        // Reuse shared query helper from waddles routes to avoid SQL duplication.
        const PAGE_SIZE: usize = 200;
        let mut offset = 0usize;
        let mut result = Vec::new();

        loop {
            let page = list_user_waddles_from_db(self.db.as_ref(), user_id, PAGE_SIZE, offset)
                .await
                .map_err(|e| {
                    warn!(user_id = %user_id, error = %e, "Failed to list user waddles");
                    XmppError::internal(format!("Failed to list user waddles: {}", e))
                })?;

            let page_len = page.len();
            result.extend(page.into_iter().map(|w| waddle_xmpp::WaddleInfo {
                id: w.id,
                name: w.name,
            }));

            if page_len < PAGE_SIZE {
                break;
            }
            offset += PAGE_SIZE;
        }

        debug!(user_id = %user_id, count = result.len(), "Found user waddles");
        Ok(result)
    }

    /// List all channels in a waddle.
    async fn list_waddle_channels(
        &self,
        waddle_id: &str,
    ) -> Result<Vec<waddle_xmpp::ChannelInfo>, XmppError> {
        debug!(waddle_id = %waddle_id, "Listing waddle channels for auto-join");

        let db_pool = self.db_pool.as_ref().ok_or_else(|| {
            warn!("Database pool not configured for auto-join channel enumeration");
            XmppError::internal("Database pool not configured".to_string())
        })?;

        let waddle_db = db_pool.get_waddle_db(waddle_id).await.map_err(|e| {
            warn!(waddle_id = %waddle_id, error = %e, "Failed to get waddle database");
            XmppError::internal(format!("Failed to access waddle database: {}", e))
        })?;

        // Ensure migrations are run on the waddle DB
        let runner = MigrationRunner::waddle();
        if let Err(e) = runner.run(&waddle_db).await {
            warn!(waddle_id = %waddle_id, error = %e, "Failed to run waddle DB migrations");
            // Continue anyway - table may already exist
        }

        // Reuse shared query helper from channels routes to avoid SQL duplication.
        const PAGE_SIZE: usize = 200;
        let mut offset = 0usize;
        let mut result = Vec::new();

        loop {
            let page = list_channels_for_waddle_from_db(&waddle_db, waddle_id, PAGE_SIZE, offset)
                .await
                .map_err(|e| {
                    warn!(waddle_id = %waddle_id, error = %e, "Failed to list channels");
                    XmppError::internal(format!("Failed to list channels: {}", e))
                })?;

            let page_len = page.len();
            result.extend(page.into_iter().map(|c| waddle_xmpp::ChannelInfo {
                id: c.id,
                name: c.name,
                channel_type: c.channel_type,
            }));

            if page_len < PAGE_SIZE {
                break;
            }
            offset += PAGE_SIZE;
        }

        debug!(waddle_id = %waddle_id, count = result.len(), "Found waddle channels");
        Ok(result)
    }
}

// =========================================================================
// Roster Conversion Helpers
// =========================================================================

/// Convert a database roster item row to a waddle_xmpp RosterItem.
fn row_to_roster_item(
    row: &crate::db::roster::RosterItemRow,
) -> Result<waddle_xmpp::roster::RosterItem, String> {
    let jid: jid::BareJid = row
        .contact_jid
        .parse()
        .map_err(|e| format!("Invalid JID '{}': {:?}", row.contact_jid, e))?;

    let subscription = waddle_xmpp::roster::Subscription::from_str(&row.subscription)
        .map_err(|e| format!("Invalid subscription '{}': {}", row.subscription, e))?;

    let ask = match &row.ask {
        Some(a) => Some(
            waddle_xmpp::roster::AskType::from_str(a)
                .map_err(|e| format!("Invalid ask '{}': {}", a, e))?,
        ),
        None => None,
    };

    Ok(waddle_xmpp::roster::RosterItem {
        jid,
        name: row.name.clone(),
        subscription,
        ask,
        groups: row.groups.clone(),
    })
}

/// Convert a waddle_xmpp RosterItem to a database roster item row.
fn roster_item_to_row(item: &waddle_xmpp::roster::RosterItem) -> crate::db::roster::RosterItemRow {
    crate::db::roster::RosterItemRow {
        contact_jid: item.jid.to_string(),
        name: item.name.clone(),
        subscription: item.subscription.as_str().to_string(),
        ask: item.ask.map(|a| a.as_str().to_string()),
        groups: item.groups.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MigrationRunner;
    use crate::permissions::ObjectType;
    use waddle_xmpp::AppState;

    async fn create_test_db() -> Arc<Database> {
        let db = Database::in_memory("test-xmpp-state")
            .await
            .expect("Failed to create test database");
        let db = Arc::new(db);

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(&db).await.expect("Failed to run migrations");

        db
    }

    #[tokio::test]
    async fn test_xmpp_state_creation() {
        let db = create_test_db().await;
        let state = XmppAppState::new("waddle.social".to_string(), db, None);

        assert_eq!(state.domain(), "waddle.social");
    }

    #[tokio::test]
    async fn test_parse_resource() {
        let obj = XmppAppState::parse_resource("waddle:penguin-club").expect("Failed to parse");
        assert_eq!(obj.object_type, ObjectType::Waddle);
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
}
