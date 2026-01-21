//! Session Management for ATProto Authentication
//!
//! This module handles:
//! - Session creation and storage
//! - Session retrieval and validation
//! - Token encryption at rest
//! - Session expiration management

use super::atproto::TokenResponse;
use super::AuthError;
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Arc;
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use crate::db::Database;

type HmacSha256 = Hmac<Sha256>;

/// Represents an authenticated user session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session ID
    pub id: String,

    /// User's DID
    pub did: String,

    /// User's handle
    pub handle: String,

    /// Access token (encrypted at rest)
    pub access_token: String,

    /// Refresh token (encrypted at rest, optional)
    pub refresh_token: Option<String>,

    /// Token endpoint URL for refresh
    pub token_endpoint: String,

    /// PDS URL
    pub pds_url: String,

    /// When the access token expires
    pub expires_at: Option<DateTime<Utc>>,

    /// When the session was created
    pub created_at: DateTime<Utc>,

    /// When the session was last used
    pub last_used_at: DateTime<Utc>,
}

impl Session {
    /// Create a new session from OAuth tokens
    pub fn from_token_response(
        did: &str,
        handle: &str,
        tokens: &TokenResponse,
        token_endpoint: &str,
        pds_url: &str,
    ) -> Self {
        let expires_at = tokens
            .expires_in
            .map(|secs| Utc::now() + Duration::seconds(secs as i64));

        Self {
            id: Uuid::new_v4().to_string(),
            did: did.to_string(),
            handle: handle.to_string(),
            access_token: tokens.access_token.clone(),
            refresh_token: tokens.refresh_token.clone(),
            token_endpoint: token_endpoint.to_string(),
            pds_url: pds_url.to_string(),
            expires_at,
            created_at: Utc::now(),
            last_used_at: Utc::now(),
        }
    }

    /// Check if the session's access token has expired
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(expires_at) => Utc::now() >= expires_at,
            None => false, // No expiration set
        }
    }

    /// Check if the session needs token refresh (expires within 5 minutes)
    pub fn needs_refresh(&self) -> bool {
        match self.expires_at {
            Some(expires_at) => Utc::now() + Duration::minutes(5) >= expires_at,
            None => false,
        }
    }

    /// Update the session with new tokens
    pub fn update_tokens(&mut self, tokens: &TokenResponse) {
        self.access_token = tokens.access_token.clone();
        if let Some(refresh_token) = &tokens.refresh_token {
            self.refresh_token = Some(refresh_token.clone());
        }
        self.expires_at = tokens
            .expires_in
            .map(|secs| Utc::now() + Duration::seconds(secs as i64));
        self.last_used_at = Utc::now();
    }
}

/// Session manager for storing and retrieving sessions
pub struct SessionManager {
    db: Arc<Database>,
    encryption_key: Option<Vec<u8>>,
}

impl SessionManager {
    /// Create a new session manager
    ///
    /// # Arguments
    ///
    /// * `db` - Database connection for session storage
    /// * `encryption_key` - Optional key for encrypting tokens at rest
    pub fn new(db: Arc<Database>, encryption_key: Option<&[u8]>) -> Self {
        Self {
            db,
            encryption_key: encryption_key.map(|k| k.to_vec()),
        }
    }

    /// Encrypt a value using HMAC-based encryption
    ///
    /// Note: This is a simple XOR-based encryption for demonstration.
    /// In production, use a proper encryption library like `aes-gcm`.
    fn encrypt(&self, value: &str) -> String {
        match &self.encryption_key {
            Some(key) => {
                let mut mac =
                    HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
                mac.update(b"token_encryption");
                let key_stream = mac.finalize().into_bytes();

                let encrypted: Vec<u8> = value
                    .bytes()
                    .zip(key_stream.iter().cycle())
                    .map(|(b, k)| b ^ k)
                    .collect();

                STANDARD.encode(&encrypted)
            }
            None => value.to_string(),
        }
    }

    /// Decrypt a value
    fn decrypt(&self, encrypted: &str) -> Result<String, AuthError> {
        match &self.encryption_key {
            Some(key) => {
                let encrypted_bytes = STANDARD.decode(encrypted).map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to decode encrypted value: {}", e))
                })?;

                let mut mac =
                    HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
                mac.update(b"token_encryption");
                let key_stream = mac.finalize().into_bytes();

                let decrypted: Vec<u8> = encrypted_bytes
                    .iter()
                    .zip(key_stream.iter().cycle())
                    .map(|(b, k)| b ^ k)
                    .collect();

                String::from_utf8(decrypted).map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to decrypt value: {}", e))
                })
            }
            None => Ok(encrypted.to_string()),
        }
    }

    /// Store a new session in the database
    #[instrument(skip(self, session))]
    pub async fn create_session(&self, session: &Session) -> Result<(), AuthError> {
        debug!("Creating session for DID: {}", session.did);

        // First, ensure the user exists
        let user_id = self.ensure_user_exists(&session.did, &session.handle).await?;

        // Encrypt tokens
        let encrypted_access = self.encrypt(&session.access_token);
        let encrypted_refresh = session
            .refresh_token
            .as_ref()
            .map(|t| self.encrypt(t));

        let expires_at = session
            .expires_at
            .map(|dt| dt.to_rfc3339());

        // Use persistent connection for in-memory databases
        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;

            conn.execute(
                r#"
                INSERT INTO sessions (
                    id, user_id, access_token, refresh_token, expires_at,
                    created_at, last_used_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                "#,
                libsql::params![
                    session.id.clone(),
                    user_id,
                    encrypted_access.clone(),
                    encrypted_refresh.clone(),
                    expires_at.clone(),
                    session.created_at.to_rfc3339(),
                    session.last_used_at.to_rfc3339()
                ],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to insert session: {}", e)))?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;

            conn.execute(
                r#"
                INSERT INTO sessions (
                    id, user_id, access_token, refresh_token, expires_at,
                    created_at, last_used_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                "#,
                libsql::params![
                    session.id.clone(),
                    user_id,
                    encrypted_access,
                    encrypted_refresh,
                    expires_at,
                    session.created_at.to_rfc3339(),
                    session.last_used_at.to_rfc3339()
                ],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to insert session: {}", e)))?;
        }

        debug!("Session created: {}", session.id);
        Ok(())
    }

    /// Ensure a user exists in the database, creating if necessary
    async fn ensure_user_exists(&self, did: &str, handle: &str) -> Result<i64, AuthError> {
        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;

            // Try to find existing user
            let mut rows = conn
                .query("SELECT id FROM users WHERE did = ?", libsql::params![did])
                .await
                .map_err(|e| AuthError::DatabaseError(format!("Failed to query user: {}", e)))?;

            if let Some(row) = rows.next().await.map_err(|e| {
                AuthError::DatabaseError(format!("Failed to read user row: {}", e))
            })? {
                let id: i64 = row.get(0).map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to get user id: {}", e))
                })?;
                return Ok(id);
            }

            // Create new user
            conn.execute(
                "INSERT INTO users (did, handle) VALUES (?, ?)",
                libsql::params![did, handle],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to insert user: {}", e)))?;

            // Get the new user ID
            let mut rows = conn
                .query("SELECT id FROM users WHERE did = ?", libsql::params![did])
                .await
                .map_err(|e| AuthError::DatabaseError(format!("Failed to query new user: {}", e)))?;

            let row = rows
                .next()
                .await
                .map_err(|e| AuthError::DatabaseError(format!("Failed to read new user row: {}", e)))?
                .ok_or_else(|| AuthError::DatabaseError("User not found after insert".to_string()))?;

            let id: i64 = row.get(0).map_err(|e| {
                AuthError::DatabaseError(format!("Failed to get new user id: {}", e))
            })?;

            Ok(id)
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;

            // Try to find existing user
            let mut rows = conn
                .query("SELECT id FROM users WHERE did = ?", libsql::params![did])
                .await
                .map_err(|e| AuthError::DatabaseError(format!("Failed to query user: {}", e)))?;

            if let Some(row) = rows.next().await.map_err(|e| {
                AuthError::DatabaseError(format!("Failed to read user row: {}", e))
            })? {
                let id: i64 = row.get(0).map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to get user id: {}", e))
                })?;
                return Ok(id);
            }

            // Create new user
            conn.execute(
                "INSERT INTO users (did, handle) VALUES (?, ?)",
                libsql::params![did, handle],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to insert user: {}", e)))?;

            // Get the new user ID
            let mut rows = conn
                .query("SELECT id FROM users WHERE did = ?", libsql::params![did])
                .await
                .map_err(|e| AuthError::DatabaseError(format!("Failed to query new user: {}", e)))?;

            let row = rows
                .next()
                .await
                .map_err(|e| AuthError::DatabaseError(format!("Failed to read new user row: {}", e)))?
                .ok_or_else(|| AuthError::DatabaseError("User not found after insert".to_string()))?;

            let id: i64 = row.get(0).map_err(|e| {
                AuthError::DatabaseError(format!("Failed to get new user id: {}", e))
            })?;

            Ok(id)
        }
    }

    /// Get a session by ID
    #[instrument(skip(self))]
    pub async fn get_session(&self, session_id: &str) -> Result<Option<Session>, AuthError> {
        debug!("Looking up session: {}", session_id);

        let query = r#"
            SELECT s.id, s.access_token, s.refresh_token, s.expires_at,
                   s.created_at, s.last_used_at, u.did, u.handle
            FROM sessions s
            JOIN users u ON s.user_id = u.id
            WHERE s.id = ?
        "#;

        let row = if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            let mut rows = conn
                .query(query, libsql::params![session_id])
                .await
                .map_err(|e| AuthError::DatabaseError(format!("Failed to query session: {}", e)))?;

            rows.next().await.map_err(|e| {
                AuthError::DatabaseError(format!("Failed to read session row: {}", e))
            })?
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;

            let mut rows = conn
                .query(query, libsql::params![session_id])
                .await
                .map_err(|e| AuthError::DatabaseError(format!("Failed to query session: {}", e)))?;

            rows.next().await.map_err(|e| {
                AuthError::DatabaseError(format!("Failed to read session row: {}", e))
            })?
        };

        match row {
            Some(row) => {
                let session = self.row_to_session(&row)?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    /// Get a session by user DID
    #[instrument(skip(self))]
    pub async fn get_session_by_did(&self, did: &str) -> Result<Option<Session>, AuthError> {
        debug!("Looking up session for DID: {}", did);

        let query = r#"
            SELECT s.id, s.access_token, s.refresh_token, s.expires_at,
                   s.created_at, s.last_used_at, u.did, u.handle
            FROM sessions s
            JOIN users u ON s.user_id = u.id
            WHERE u.did = ?
            ORDER BY s.last_used_at DESC
            LIMIT 1
        "#;

        let row = if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            let mut rows = conn
                .query(query, libsql::params![did])
                .await
                .map_err(|e| AuthError::DatabaseError(format!("Failed to query session: {}", e)))?;

            rows.next().await.map_err(|e| {
                AuthError::DatabaseError(format!("Failed to read session row: {}", e))
            })?
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;

            let mut rows = conn
                .query(query, libsql::params![did])
                .await
                .map_err(|e| AuthError::DatabaseError(format!("Failed to query session: {}", e)))?;

            rows.next().await.map_err(|e| {
                AuthError::DatabaseError(format!("Failed to read session row: {}", e))
            })?
        };

        match row {
            Some(row) => {
                let session = self.row_to_session(&row)?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    /// Convert a database row to a Session
    fn row_to_session(&self, row: &libsql::Row) -> Result<Session, AuthError> {
        let id: String = row.get(0).map_err(|e| {
            AuthError::DatabaseError(format!("Failed to get session id: {}", e))
        })?;

        let encrypted_access: String = row.get(1).map_err(|e| {
            AuthError::DatabaseError(format!("Failed to get access token: {}", e))
        })?;

        let encrypted_refresh: Option<String> = row.get(2).ok();

        let expires_at_str: Option<String> = row.get(3).ok();
        let created_at_str: String = row.get(4).map_err(|e| {
            AuthError::DatabaseError(format!("Failed to get created_at: {}", e))
        })?;
        let last_used_at_str: String = row.get(5).map_err(|e| {
            AuthError::DatabaseError(format!("Failed to get last_used_at: {}", e))
        })?;

        let did: String = row.get(6).map_err(|e| {
            AuthError::DatabaseError(format!("Failed to get DID: {}", e))
        })?;
        let handle: String = row.get(7).map_err(|e| {
            AuthError::DatabaseError(format!("Failed to get handle: {}", e))
        })?;

        // Decrypt tokens
        let access_token = self.decrypt(&encrypted_access)?;
        let refresh_token = match encrypted_refresh {
            Some(enc) => Some(self.decrypt(&enc)?),
            None => None,
        };

        // Parse timestamps
        let expires_at = expires_at_str
            .map(|s| DateTime::parse_from_rfc3339(&s).map(|dt| dt.with_timezone(&Utc)))
            .transpose()
            .map_err(|e| AuthError::DatabaseError(format!("Failed to parse expires_at: {}", e)))?;

        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| AuthError::DatabaseError(format!("Failed to parse created_at: {}", e)))?;

        let last_used_at = DateTime::parse_from_rfc3339(&last_used_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|e| AuthError::DatabaseError(format!("Failed to parse last_used_at: {}", e)))?;

        Ok(Session {
            id,
            did,
            handle,
            access_token,
            refresh_token,
            token_endpoint: String::new(), // Would need to store this in DB
            pds_url: String::new(),        // Would need to store this in DB
            expires_at,
            created_at,
            last_used_at,
        })
    }

    /// Update session's last_used_at timestamp
    #[instrument(skip(self))]
    pub async fn touch_session(&self, session_id: &str) -> Result<(), AuthError> {
        let now = Utc::now().to_rfc3339();

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(
                "UPDATE sessions SET last_used_at = ? WHERE id = ?",
                libsql::params![now, session_id],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to update session: {}", e)))?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;

            conn.execute(
                "UPDATE sessions SET last_used_at = ? WHERE id = ?",
                libsql::params![now, session_id],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to update session: {}", e)))?;
        }

        Ok(())
    }

    /// Delete a session
    #[instrument(skip(self))]
    pub async fn delete_session(&self, session_id: &str) -> Result<(), AuthError> {
        debug!("Deleting session: {}", session_id);

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(
                "DELETE FROM sessions WHERE id = ?",
                libsql::params![session_id],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to delete session: {}", e)))?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;

            conn.execute(
                "DELETE FROM sessions WHERE id = ?",
                libsql::params![session_id],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to delete session: {}", e)))?;
        }

        Ok(())
    }

    /// Delete all expired sessions
    #[instrument(skip(self))]
    pub async fn cleanup_expired_sessions(&self) -> Result<usize, AuthError> {
        let now = Utc::now().to_rfc3339();

        let deleted = if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(
                "DELETE FROM sessions WHERE expires_at IS NOT NULL AND expires_at < ?",
                libsql::params![now],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to cleanup sessions: {}", e)))?
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect to database: {}", e))
            })?;

            conn.execute(
                "DELETE FROM sessions WHERE expires_at IS NOT NULL AND expires_at < ?",
                libsql::params![now],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to cleanup sessions: {}", e)))?
        };

        if deleted > 0 {
            debug!("Cleaned up {} expired sessions", deleted);
        }

        Ok(deleted as usize)
    }

    /// Validate a session for XMPP authentication
    ///
    /// Returns the session if it exists and is not expired.
    #[instrument(skip(self))]
    pub async fn validate_session(&self, session_id: &str) -> Result<Session, AuthError> {
        let session = self
            .get_session(session_id)
            .await?
            .ok_or_else(|| AuthError::SessionNotFound(session_id.to_string()))?;

        if session.is_expired() {
            warn!("Session {} is expired", session_id);
            return Err(AuthError::SessionExpired);
        }

        // Touch the session to update last_used_at
        self.touch_session(session_id).await?;

        Ok(session)
    }
}

/// Pending authorization request stored temporarily during OAuth flow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAuthorization {
    /// State parameter (key for lookup)
    pub state: String,

    /// PKCE code verifier
    pub code_verifier: String,

    /// User's DID
    pub did: String,

    /// User's handle
    pub handle: String,

    /// Token endpoint URL
    pub token_endpoint: String,

    /// PDS URL
    pub pds_url: String,

    /// When this request was created
    pub created_at: DateTime<Utc>,
}

impl PendingAuthorization {
    /// Create a new pending authorization from an authorization request
    pub fn from_authorization_request(
        request: &super::atproto::AuthorizationRequest,
    ) -> Self {
        Self {
            state: request.state.clone(),
            code_verifier: request.code_verifier.clone(),
            did: request.did.clone(),
            handle: request.handle.clone(),
            token_endpoint: request.token_endpoint.clone(),
            pds_url: request.pds_url.clone(),
            created_at: Utc::now(),
        }
    }

    /// Check if this pending authorization has expired (5 minute timeout)
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.created_at + Duration::minutes(5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_expiration() {
        let mut session = Session {
            id: "test".to_string(),
            did: "did:plc:test".to_string(),
            handle: "test.bsky.social".to_string(),
            access_token: "token".to_string(),
            refresh_token: None,
            token_endpoint: "https://bsky.social/oauth/token".to_string(),
            pds_url: "https://bsky.social".to_string(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            created_at: Utc::now(),
            last_used_at: Utc::now(),
        };

        assert!(!session.is_expired());
        assert!(!session.needs_refresh());

        // Set to expire in 2 minutes
        session.expires_at = Some(Utc::now() + Duration::minutes(2));
        assert!(!session.is_expired());
        assert!(session.needs_refresh()); // Within 5 minute window

        // Set to expired
        session.expires_at = Some(Utc::now() - Duration::minutes(1));
        assert!(session.is_expired());
    }

    #[test]
    fn test_encryption_roundtrip() {
        use crate::db::Database;

        // We can't actually test with a database in a sync test,
        // but we can test the encryption functions directly
        let key = b"test-encryption-key-32-bytes!!!";

        // Create manager with mock db (won't be used)
        let manager = SessionManager {
            db: Arc::new(futures::executor::block_on(async {
                Database::in_memory("test").await.unwrap()
            })),
            encryption_key: Some(key.to_vec()),
        };

        let original = "my-secret-token";
        let encrypted = manager.encrypt(original);
        let decrypted = manager.decrypt(&encrypted).unwrap();

        assert_eq!(original, decrypted);
        assert_ne!(original, encrypted);
    }

    #[test]
    fn test_pending_authorization_expiration() {
        let pending = PendingAuthorization {
            state: "test-state".to_string(),
            code_verifier: "test-verifier".to_string(),
            did: "did:plc:test".to_string(),
            handle: "test.bsky.social".to_string(),
            token_endpoint: "https://bsky.social/oauth/token".to_string(),
            pds_url: "https://bsky.social".to_string(),
            created_at: Utc::now(),
        };

        assert!(!pending.is_expired());

        let expired_pending = PendingAuthorization {
            created_at: Utc::now() - Duration::minutes(10),
            ..pending
        };

        assert!(expired_pending.is_expired());
    }
}
