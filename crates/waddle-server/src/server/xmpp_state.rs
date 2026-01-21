//! XMPP AppState implementation bridging to waddle-server services.
//!
//! This module implements the `waddle_xmpp::AppState` trait by delegating to
//! the existing auth, session, and permission services in waddle-server.

use std::sync::Arc;

use tracing::{debug, warn};
use waddle_xmpp::{Session as XmppSession, XmppError};

use crate::auth::{jid_to_did, SessionManager};
use crate::db::Database;
use crate::permissions::{Object, PermissionService, Subject};

/// XMPP application state that bridges to waddle-server services.
///
/// This struct implements `waddle_xmpp::AppState` by delegating to:
/// - `SessionManager` for session validation
/// - `PermissionService` for permission checks
pub struct XmppAppState {
    /// The XMPP server domain (e.g., "waddle.social")
    domain: String,
    /// Session manager for validating XMPP authentication tokens
    session_manager: SessionManager,
    /// Permission service for authorization checks
    permission_service: PermissionService,
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

        Self {
            domain,
            session_manager,
            permission_service,
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

    /// Get the XMPP server domain.
    fn domain(&self) -> &str {
        &self.domain
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
