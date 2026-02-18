//! Native user authentication storage for XEP-0077 In-Band Registration.
//!
//! This module provides storage and verification for native XMPP users who
//! authenticate via SCRAM-SHA-256 rather than ATProto OAuth. Native users
//! can be registered via XEP-0077 In-Band Registration.
//!
//! ## Security Model
//!
//! - Passwords are hashed using Argon2id (memory-hard, recommended by OWASP)
//! - SCRAM keys (StoredKey, ServerKey) are derived and stored for authentication
//! - Plaintext passwords are never stored
//! - Each user has a unique random salt

use std::sync::Arc;

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use base64::prelude::*;
use tracing::debug;
use waddle_xmpp::ScramCredentials;

use crate::db::Database;

use super::AuthError;

/// Default PBKDF2 iteration count for SCRAM key derivation.
/// 4096 is the minimum recommended by RFC 7677.
pub const DEFAULT_SCRAM_ITERATIONS: u32 = 4096;

/// Native user record from the database.
#[derive(Debug, Clone)]
pub struct NativeUser {
    /// Database ID
    pub id: i64,
    /// Username (local part of JID)
    pub username: String,
    /// Domain (domain part of JID)
    pub domain: String,
    /// Argon2id password hash
    pub password_hash: String,
    /// SCRAM salt (base64 encoded)
    pub salt: String,
    /// PBKDF2 iterations for SCRAM
    pub iterations: u32,
    /// SCRAM StoredKey (raw bytes)
    pub stored_key: Vec<u8>,
    /// SCRAM ServerKey (raw bytes)
    pub server_key: Vec<u8>,
    /// Optional email for recovery
    pub email: Option<String>,
    /// Creation timestamp
    pub created_at: String,
    /// Last update timestamp
    pub updated_at: String,
}

/// Request to register a new native user via XEP-0077.
#[derive(Debug, Clone)]
pub struct RegisterRequest {
    /// Desired username (local part of JID)
    pub username: String,
    /// Domain (typically the server domain)
    pub domain: String,
    /// Plaintext password (will be hashed)
    pub password: String,
    /// Optional email for recovery
    pub email: Option<String>,
}

/// Native user store for XEP-0077 registration and SCRAM authentication.
#[derive(Clone)]
pub struct NativeUserStore {
    /// Database connection
    db: Arc<Database>,
}

impl NativeUserStore {
    /// Create a new native user store.
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Get a database connection.
    ///
    /// For in-memory databases, this returns a guard to the persistent connection
    /// to ensure data consistency (libSQL creates isolated databases for each `:memory:` connection).
    /// For file-based databases, we create new connections.
    async fn get_connection(&self) -> Result<ConnectionGuard<'_>, AuthError> {
        if let Some(persistent) = self.db.persistent_connection() {
            let guard = persistent.lock().await;
            Ok(ConnectionGuard::Persistent(guard))
        } else {
            let conn = self
                .db
                .connect()
                .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
            Ok(ConnectionGuard::Owned(conn))
        }
    }

    /// Register a new native user.
    ///
    /// This creates the user with:
    /// - Argon2id password hash
    /// - SCRAM-SHA-256 keys (StoredKey, ServerKey)
    /// - Random salt
    ///
    /// Returns the user ID on success.
    pub async fn register(&self, request: RegisterRequest) -> Result<i64, AuthError> {
        // Validate username format (must be valid JID localpart)
        validate_username(&request.username)?;

        // Check if username already exists
        if self.user_exists(&request.username, &request.domain).await? {
            return Err(AuthError::UserAlreadyExists(request.username));
        }

        // Generate Argon2id hash
        let argon2 = Argon2::default();
        let salt = SaltString::generate(&mut OsRng);
        let password_hash = argon2
            .hash_password(request.password.as_bytes(), &salt)
            .map_err(|e| AuthError::CryptoError(format!("Failed to hash password: {}", e)))?
            .to_string();

        // Generate SCRAM salt and keys
        let scram_salt = generate_scram_salt();
        let scram_salt_b64 = BASE64_STANDARD.encode(&scram_salt);
        let (stored_key, server_key) = waddle_xmpp::auth::scram::generate_scram_keys(
            &request.password,
            &scram_salt,
            DEFAULT_SCRAM_ITERATIONS,
        );

        // Insert into database
        let conn = self.get_connection().await?;

        let email_str = request.email.as_deref();
        conn.as_ref()
            .execute(
                r#"
                INSERT INTO native_users (username, domain, password_hash, salt, iterations, stored_key, server_key, email)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                (
                    request.username.as_str(),
                    request.domain.as_str(),
                    password_hash.as_str(),
                    scram_salt_b64.as_str(),
                    DEFAULT_SCRAM_ITERATIONS as i64,
                    stored_key.as_slice(),
                    server_key.as_slice(),
                    email_str,
                ),
            )
            .await
            .map_err(db_err)?;

        let user_id = conn.as_ref().last_insert_rowid();

        debug!(
            username = %request.username,
            domain = %request.domain,
            user_id = user_id,
            "Native user registered"
        );

        Ok(user_id)
    }

    /// Check if a username exists in the given domain.
    pub async fn user_exists(&self, username: &str, domain: &str) -> Result<bool, AuthError> {
        let conn = self.get_connection().await?;

        let mut rows = conn
            .as_ref()
            .query(
                "SELECT 1 FROM native_users WHERE username = ? AND domain = ?",
                (username, domain),
            )
            .await
            .map_err(db_err)?;

        Ok(rows.next().await.map_err(db_err)?.is_some())
    }

    /// Get SCRAM credentials for a user.
    pub async fn get_scram_credentials(
        &self,
        username: &str,
        domain: &str,
    ) -> Result<Option<ScramCredentials>, AuthError> {
        let conn = self.get_connection().await?;

        let mut rows = conn
            .as_ref()
            .query(
                r#"
                SELECT salt, iterations, stored_key, server_key
                FROM native_users
                WHERE username = ? AND domain = ?
                "#,
                (username, domain),
            )
            .await
            .map_err(db_err)?;

        match rows.next().await.map_err(db_err)? {
            Some(row) => {
                let iterations: i64 = row.get(1).map_err(db_err)?;
                Ok(Some(ScramCredentials {
                    salt_b64: row.get(0).map_err(db_err)?,
                    iterations: iterations as u32,
                    stored_key: row.get(2).map_err(db_err)?,
                    server_key: row.get(3).map_err(db_err)?,
                }))
            }
            None => Ok(None),
        }
    }

    /// Verify a password for a native user using Argon2id.
    pub async fn verify_password(
        &self,
        username: &str,
        domain: &str,
        password: &str,
    ) -> Result<bool, AuthError> {
        let conn = self.get_connection().await?;

        let mut rows = conn
            .as_ref()
            .query(
                "SELECT password_hash FROM native_users WHERE username = ? AND domain = ?",
                (username, domain),
            )
            .await
            .map_err(db_err)?;

        match rows.next().await.map_err(db_err)? {
            Some(row) => {
                let hash_str: String = row.get(0).map_err(db_err)?;
                let parsed_hash = PasswordHash::new(&hash_str)
                    .map_err(|e| AuthError::CryptoError(format!("Invalid password hash: {}", e)))?;
                Ok(Argon2::default()
                    .verify_password(password.as_bytes(), &parsed_hash)
                    .is_ok())
            }
            None => Ok(false),
        }
    }

    /// Get a native user by username and domain.
    pub async fn get_user(
        &self,
        username: &str,
        domain: &str,
    ) -> Result<Option<NativeUser>, AuthError> {
        let conn = self.get_connection().await?;

        let mut rows = conn.as_ref()
            .query(
                r#"
                SELECT id, username, domain, password_hash, salt, iterations, stored_key, server_key, email, created_at, updated_at
                FROM native_users
                WHERE username = ? AND domain = ?
                "#,
                (username, domain),
            )
            .await
            .map_err(db_err)?;

        match rows.next().await.map_err(db_err)? {
            Some(row) => {
                let iterations: i64 = row.get(5).map_err(db_err)?;
                let user = NativeUser {
                    id: row.get(0).map_err(db_err)?,
                    username: row.get(1).map_err(db_err)?,
                    domain: row.get(2).map_err(db_err)?,
                    password_hash: row.get(3).map_err(db_err)?,
                    salt: row.get(4).map_err(db_err)?,
                    iterations: iterations as u32,
                    stored_key: row.get(6).map_err(db_err)?,
                    server_key: row.get(7).map_err(db_err)?,
                    email: row.get(8).ok(),
                    created_at: row.get(9).map_err(db_err)?,
                    updated_at: row.get(10).map_err(db_err)?,
                };
                Ok(Some(user))
            }
            None => Ok(None),
        }
    }

    /// Update a user's password.
    ///
    /// This regenerates both the Argon2id hash and SCRAM keys.
    pub async fn update_password(
        &self,
        username: &str,
        domain: &str,
        new_password: &str,
    ) -> Result<(), AuthError> {
        // Generate new Argon2id hash
        let argon2 = Argon2::default();
        let salt = SaltString::generate(&mut OsRng);
        let password_hash = argon2
            .hash_password(new_password.as_bytes(), &salt)
            .map_err(|e| AuthError::CryptoError(format!("Failed to hash password: {}", e)))?
            .to_string();

        // Generate new SCRAM salt and keys
        let scram_salt = generate_scram_salt();
        let scram_salt_b64 = BASE64_STANDARD.encode(&scram_salt);
        let (stored_key, server_key) = waddle_xmpp::auth::scram::generate_scram_keys(
            new_password,
            &scram_salt,
            DEFAULT_SCRAM_ITERATIONS,
        );

        let conn = self.get_connection().await?;

        let affected = conn.as_ref()
            .execute(
                r#"
                UPDATE native_users
                SET password_hash = ?, salt = ?, stored_key = ?, server_key = ?, updated_at = datetime('now')
                WHERE username = ? AND domain = ?
                "#,
                (
                    password_hash.as_str(),
                    scram_salt_b64.as_str(),
                    stored_key.as_slice(),
                    server_key.as_slice(),
                    username,
                    domain,
                ),
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to update password: {}", e)))?;

        if affected == 0 {
            return Err(AuthError::UserNotFound(format!("{}@{}", username, domain)));
        }

        debug!(username = %username, domain = %domain, "Password updated for native user");
        Ok(())
    }

    /// Delete a native user.
    pub async fn delete_user(&self, username: &str, domain: &str) -> Result<bool, AuthError> {
        let conn = self.get_connection().await?;

        let affected = conn
            .as_ref()
            .execute(
                "DELETE FROM native_users WHERE username = ? AND domain = ?",
                (username, domain),
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to delete user: {}", e)))?;

        if affected > 0 {
            debug!(username = %username, domain = %domain, "Native user deleted");
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// A guard that wraps either a persistent connection (for in-memory databases)
/// or an owned connection (for file-based databases).
///
/// This ensures that in-memory databases always use the persistent connection
/// to maintain data across operations.
enum ConnectionGuard<'a> {
    /// Persistent connection guard for in-memory databases
    Persistent(tokio::sync::MutexGuard<'a, libsql::Connection>),
    /// Owned connection for file-based databases
    Owned(libsql::Connection),
}

impl<'a> ConnectionGuard<'a> {
    /// Get a reference to the underlying connection
    fn as_ref(&self) -> &libsql::Connection {
        match self {
            ConnectionGuard::Persistent(guard) => guard,
            ConnectionGuard::Owned(conn) => conn,
        }
    }
}

/// Generate a random SCRAM salt (16 bytes).
fn generate_scram_salt() -> Vec<u8> {
    use rand::Rng;
    let mut salt = vec![0u8; 16];
    rand::rng().fill(&mut salt[..]);
    salt
}

/// Helper to convert libsql errors to AuthError.
fn db_err<E: std::fmt::Display>(e: E) -> AuthError {
    AuthError::DatabaseError(e.to_string())
}

/// Validate a username for JID localpart compliance.
///
/// Per RFC 7622, the localpart must:
/// - Not be empty
/// - Not exceed 1023 bytes in UTF-8
/// - Not contain prohibited characters
fn validate_username(username: &str) -> Result<(), AuthError> {
    if username.is_empty() {
        return Err(AuthError::InvalidUsername(
            "Username cannot be empty".to_string(),
        ));
    }

    if username.len() > 1023 {
        return Err(AuthError::InvalidUsername("Username too long".to_string()));
    }

    // Check for prohibited characters in JID localpart
    let prohibited = ['@', '/', '"', '&', '\'', '<', '>', ' ', '\t', '\n', '\r'];
    for ch in prohibited {
        if username.contains(ch) {
            return Err(AuthError::InvalidUsername(format!(
                "Username contains prohibited character: '{}'",
                ch
            )));
        }
    }

    // Check for control characters
    for ch in username.chars() {
        if ch.is_control() {
            return Err(AuthError::InvalidUsername(
                "Username contains control characters".to_string(),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MigrationRunner;

    async fn create_test_db() -> Arc<Database> {
        let db = Database::in_memory("test-native-users")
            .await
            .expect("Failed to create test database");
        let db = Arc::new(db);

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(&db).await.expect("Failed to run migrations");

        db
    }

    #[tokio::test]
    async fn test_register_user() {
        let db = create_test_db().await;
        let store = NativeUserStore::new(db);

        let request = RegisterRequest {
            username: "alice".to_string(),
            domain: "example.com".to_string(),
            password: "secret123".to_string(),
            email: Some("alice@email.com".to_string()),
        };

        let user_id = store
            .register(request)
            .await
            .expect("Failed to register user");
        assert!(user_id > 0);

        // Verify user exists
        let exists = store.user_exists("alice", "example.com").await.unwrap();
        assert!(exists);
    }

    #[tokio::test]
    async fn test_duplicate_user() {
        let db = create_test_db().await;
        let store = NativeUserStore::new(db);

        let request = RegisterRequest {
            username: "bob".to_string(),
            domain: "example.com".to_string(),
            password: "secret123".to_string(),
            email: None,
        };

        store
            .register(request.clone())
            .await
            .expect("First registration should succeed");

        // Second registration should fail
        let result = store.register(request).await;
        assert!(matches!(result, Err(AuthError::UserAlreadyExists(_))));
    }

    #[tokio::test]
    async fn test_get_scram_credentials() {
        let db = create_test_db().await;
        let store = NativeUserStore::new(db);

        let request = RegisterRequest {
            username: "charlie".to_string(),
            domain: "example.com".to_string(),
            password: "testpassword".to_string(),
            email: None,
        };

        store.register(request).await.unwrap();

        let creds = store
            .get_scram_credentials("charlie", "example.com")
            .await
            .unwrap();

        assert!(creds.is_some());
        let creds = creds.unwrap();

        // Verify SCRAM keys are properly generated
        assert_eq!(creds.stored_key.len(), 32); // SHA-256 output
        assert_eq!(creds.server_key.len(), 32);
        assert_eq!(creds.iterations, DEFAULT_SCRAM_ITERATIONS);
        assert!(!creds.salt_b64.is_empty());
    }

    #[tokio::test]
    async fn test_verify_password() {
        let db = create_test_db().await;
        let store = NativeUserStore::new(db);

        let request = RegisterRequest {
            username: "dave".to_string(),
            domain: "example.com".to_string(),
            password: "correctpassword".to_string(),
            email: None,
        };

        store.register(request).await.unwrap();

        // Correct password should verify
        let verified = store
            .verify_password("dave", "example.com", "correctpassword")
            .await
            .unwrap();
        assert!(verified);

        // Wrong password should not verify
        let verified = store
            .verify_password("dave", "example.com", "wrongpassword")
            .await
            .unwrap();
        assert!(!verified);

        // Non-existent user should not verify
        let verified = store
            .verify_password("nonexistent", "example.com", "anypassword")
            .await
            .unwrap();
        assert!(!verified);
    }

    #[tokio::test]
    async fn test_update_password() {
        let db = create_test_db().await;
        let store = NativeUserStore::new(db);

        let request = RegisterRequest {
            username: "eve".to_string(),
            domain: "example.com".to_string(),
            password: "oldpassword".to_string(),
            email: None,
        };

        store.register(request).await.unwrap();

        // Update password
        store
            .update_password("eve", "example.com", "newpassword")
            .await
            .unwrap();

        // Old password should not work
        let verified = store
            .verify_password("eve", "example.com", "oldpassword")
            .await
            .unwrap();
        assert!(!verified);

        // New password should work
        let verified = store
            .verify_password("eve", "example.com", "newpassword")
            .await
            .unwrap();
        assert!(verified);
    }

    #[tokio::test]
    async fn test_delete_user() {
        let db = create_test_db().await;
        let store = NativeUserStore::new(db);

        let request = RegisterRequest {
            username: "frank".to_string(),
            domain: "example.com".to_string(),
            password: "password".to_string(),
            email: None,
        };

        store.register(request).await.unwrap();

        // Delete user
        let deleted = store.delete_user("frank", "example.com").await.unwrap();
        assert!(deleted);

        // User should no longer exist
        let exists = store.user_exists("frank", "example.com").await.unwrap();
        assert!(!exists);

        // Deleting again should return false
        let deleted = store.delete_user("frank", "example.com").await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn test_validate_username() {
        // Valid usernames
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("alice123").is_ok());
        assert!(validate_username("alice.bob").is_ok());
        assert!(validate_username("alice-bob").is_ok());
        assert!(validate_username("alice_bob").is_ok());

        // Invalid usernames
        assert!(validate_username("").is_err());
        assert!(validate_username("alice@bob").is_err());
        assert!(validate_username("alice/bob").is_err());
        assert!(validate_username("alice bob").is_err());
        assert!(validate_username("alice\tbob").is_err());
    }
}
