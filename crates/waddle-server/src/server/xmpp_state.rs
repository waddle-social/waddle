//! XMPP AppState implementation bridging to waddle-server services.
//!
//! This module implements the `waddle_xmpp::AppState` trait by delegating to
//! the existing auth, session, and permission services in waddle-server.

use std::sync::Arc;

use tracing::{debug, warn};
use waddle_xmpp::{Session as XmppSession, XmppError};

use crate::auth::{did_to_jid, jid_to_did, NativeUserStore, RegisterRequest, SessionManager};
use crate::db::Database;
use crate::permissions::{Object, PermissionService, Subject};
use crate::vcard::VCardStore;

/// XMPP application state that bridges to waddle-server services.
///
/// This struct implements `waddle_xmpp::AppState` by delegating to:
/// - `SessionManager` for session validation
/// - `PermissionService` for permission checks
/// - `NativeUserStore` for XEP-0077 registration and SCRAM authentication
/// - `VCardStore` for XEP-0054 vcard-temp storage
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
        }
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
    /// Subject format: "user:{did}" or "waddle:{id}#member"
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
    /// The JID's localpart is converted to a DID and verified against the session.
    async fn validate_session(
        &self,
        jid: &jid::Jid,
        token: &str,
    ) -> Result<XmppSession, XmppError> {
        debug!(jid = %jid, "Validating XMPP session");

        // Convert JID to DID for verification
        let expected_did = jid_to_did(&jid.to_string()).map_err(|e| {
            warn!(jid = %jid, error = %e, "Failed to convert JID to DID");
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

        // Verify the DID matches
        if session.did != expected_did {
            warn!(
                expected_did = %expected_did,
                session_did = %session.did,
                "DID mismatch between JID and session"
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
            did: session.did,
            jid: bare_jid,
            created_at: session.created_at,
            expires_at,
        })
    }

    /// Check if a subject has permission to perform an action on a resource.
    ///
    /// Resource format: "waddle:{id}" or "channel:{id}"
    /// Subject format: "user:{did}"
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
    /// session's DID after validation.
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

        // Convert DID to JID
        let jid_str = did_to_jid(&session.did, &self.domain).map_err(|e| {
            warn!(did = %session.did, error = %e, "Failed to convert DID to JID");
            XmppError::auth_failed(format!("Invalid DID format: {}", e))
        })?;

        let bare_jid: jid::BareJid = jid_str.parse().map_err(|e| {
            warn!(jid = %jid_str, error = ?e, "Failed to parse generated JID");
            XmppError::auth_failed(format!("Invalid JID: {:?}", e))
        })?;

        // Calculate expires_at - use session expiry or default to 24 hours from now
        let expires_at = session
            .expires_at
            .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::hours(24));

        debug!(jid = %bare_jid, did = %session.did, "OAUTHBEARER session validated");

        Ok(XmppSession {
            did: session.did,
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
        let base_url = std::env::var("WADDLE_BASE_URL")
            .unwrap_or_else(|_| format!("https://{}", self.domain));
        format!("{}/.well-known/oauth-authorization-server", base_url.trim_end_matches('/'))
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

        match self.native_user_store.get_scram_credentials(username, &self.domain).await {
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
                debug!(username = username, user_id = user_id, "Native user registered successfully");
                Ok(())
            }
            Err(crate::auth::AuthError::UserAlreadyExists(_)) => {
                warn!(username = username, "Registration failed: user already exists");
                Err(XmppError::conflict(Some(format!("User '{}' already exists", username))))
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
    async fn native_user_exists(
        &self,
        username: &str,
    ) -> Result<bool, XmppError> {
        debug!(
            username = username,
            domain = %self.domain,
            "Checking if native user exists"
        );

        match self.native_user_store.user_exists(username, &self.domain).await {
            Ok(exists) => Ok(exists),
            Err(e) => {
                warn!(username = username, error = %e, "Failed to check user existence");
                Err(XmppError::internal(format!("Database error: {}", e)))
            }
        }
    }

    /// Get the vCard for a user (XEP-0054).
    async fn get_vcard(
        &self,
        jid: &jid::BareJid,
    ) -> Result<Option<String>, XmppError> {
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
    async fn set_vcard(
        &self,
        jid: &jid::BareJid,
        vcard_xml: &str,
    ) -> Result<(), XmppError> {
        debug!(jid = %jid, "Setting vCard");

        match self.vcard_store.set(jid, vcard_xml).await {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(jid = %jid, error = %e, "Failed to set vCard");
                Err(XmppError::internal(format!("Database error: {}", e)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MigrationRunner;
    use crate::permissions::ObjectType;
    use waddle_xmpp::AppState;

    async fn create_test_db() -> Arc<Database> {
        let db = Database::in_memory("test-xmpp-state").await.expect("Failed to create test database");
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
        let subj = XmppAppState::parse_subject("user:did:plc:abc123").expect("Failed to parse");
        assert_eq!(subj.id, "did:plc:abc123");
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
