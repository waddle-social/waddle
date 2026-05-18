//! Native user authentication storage for XEP-0077 In-Band Registration.
//!
//! This module provides storage and verification for native XMPP users who
//! authenticate via SCRAM-SHA-256 rather than external OAuth/OIDC. Native users
//! can be registered via XEP-0077 In-Band Registration.
//!
//! ## Security Model
//!
//! - Passwords are hashed using Argon2id (memory-hard, recommended by OWASP)
//! - SCRAM keys (StoredKey, ServerKey) are derived and stored for authentication
//! - Plaintext passwords are never stored
//! - Each user has a unique random salt

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
    Argon2,
};
use base64::prelude::*;
use kameo::actor::ActorRef;
use tracing::debug;
use waddle_xmpp::ScramCredentials;

use crate::db::actor::{DbActor, DbExecute, DbQuery, DbQueryOne};
use crate::db::{row_value, ValueExt};

use super::AuthError;

/// Default PBKDF2 iteration count for SCRAM key derivation.
/// 4096 is the minimum recommended by RFC 7677.
pub const DEFAULT_SCRAM_ITERATIONS: u32 = 4096;

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
    /// Database actor
    actor: ActorRef<DbActor>,
}

impl NativeUserStore {
    /// Create a new native user store.
    pub fn new(actor: ActorRef<DbActor>) -> Self {
        Self { actor }
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
        let email_str = request.email.as_deref();
        let rows = self
            .actor
            .ask(DbQuery {
                sql: r#"
                    INSERT INTO native_users (username, domain, password_hash, salt, iterations, stored_key, server_key, email)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    RETURNING id
                "#
                .to_string(),
                params: vec![
                    request.username.as_str().into(),
                    request.domain.as_str().into(),
                    password_hash.into(),
                    scram_salt_b64.into(),
                    i64::from(DEFAULT_SCRAM_ITERATIONS).into(),
                    stored_key.into(),
                    server_key.into(),
                    email_str.into(),
                ],
            })
            .await
            .map_err(db_err)?;

        let user_id: i64 = rows
            .into_iter()
            .next()
            .ok_or_else(|| AuthError::DatabaseError("insert did not return id".to_string()))?
            .first()
            .cloned()
            .ok_or_else(|| AuthError::DatabaseError("insert did not return id".to_string()))
            .and_then(|value| match value {
                crate::db::Value::Integer(v) => Ok(v),
                other => Err(AuthError::DatabaseError(format!(
                    "insert returned unexpected id type: {:?}",
                    other
                ))),
            })?;

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
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: "SELECT 1 FROM native_users WHERE username = ? AND domain = ?".to_string(),
                params: vec![username.into(), domain.into()],
            })
            .await
            .map_err(db_err)?;

        Ok(row.is_some())
    }

    /// Get SCRAM credentials for a user.
    pub async fn get_scram_credentials(
        &self,
        username: &str,
        domain: &str,
    ) -> Result<Option<ScramCredentials>, AuthError> {
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: r#"
                    SELECT salt, iterations, stored_key, server_key
                    FROM native_users
                    WHERE username = ? AND domain = ?
                "#
                .to_string(),
                params: vec![username.into(), domain.into()],
            })
            .await
            .map_err(db_err)?;

        match row {
            Some(row) => {
                let iterations = match row_value(&row, 1).map_err(db_err)? {
                    crate::db::Value::Integer(value) => *value,
                    other => {
                        return Err(AuthError::DatabaseError(format!(
                            "invalid iterations value: {:?}",
                            other
                        )));
                    }
                };
                let salt_b64 = row_value(&row, 0)
                    .and_then(ValueExt::as_string)
                    .map_err(db_err)?;
                let stored_key = match row_value(&row, 2).map_err(db_err)? {
                    crate::db::Value::Blob(value) => value.clone(),
                    other => {
                        return Err(AuthError::DatabaseError(format!(
                            "invalid stored_key value: {:?}",
                            other
                        )));
                    }
                };
                let server_key = match row_value(&row, 3).map_err(db_err)? {
                    crate::db::Value::Blob(value) => value.clone(),
                    other => {
                        return Err(AuthError::DatabaseError(format!(
                            "invalid server_key value: {:?}",
                            other
                        )));
                    }
                };
                Ok(Some(ScramCredentials {
                    salt_b64,
                    iterations: iterations as u32,
                    stored_key,
                    server_key,
                }))
            }
            None => Ok(None),
        }
    }

    /// Verify a password for a native user using Argon2id.
    #[cfg(test)]
    pub async fn verify_password(
        &self,
        username: &str,
        domain: &str,
        password: &str,
    ) -> Result<bool, AuthError> {
        use argon2::password_hash::PasswordVerifier;

        let row = self
            .actor
            .ask(DbQueryOne {
                sql: "SELECT password_hash FROM native_users WHERE username = ? AND domain = ?"
                    .to_string(),
                params: vec![username.into(), domain.into()],
            })
            .await
            .map_err(db_err)?;

        match row {
            Some(row) => {
                let hash_str = row_value(&row, 0)
                    .and_then(ValueExt::as_string)
                    .map_err(db_err)?;
                let parsed_hash = argon2::password_hash::PasswordHash::new(&hash_str)
                    .map_err(|e| AuthError::CryptoError(format!("Invalid password hash: {}", e)))?;
                Ok(Argon2::default()
                    .verify_password(password.as_bytes(), &parsed_hash)
                    .is_ok())
            }
            None => Ok(false),
        }
    }

    /// Update a user's password.
    ///
    /// This regenerates both the Argon2id hash and SCRAM keys.
    #[cfg(test)]
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

        let affected = self
            .actor
            .ask(DbExecute {
                sql: r#"
                    UPDATE native_users
                    SET password_hash = ?, salt = ?, stored_key = ?, server_key = ?, updated_at = datetime('now')
                    WHERE username = ? AND domain = ?
                "#
                .to_string(),
                params: vec![
                    password_hash.into(),
                    scram_salt_b64.into(),
                    stored_key.into(),
                    server_key.into(),
                    username.into(),
                    domain.into(),
                ],
            })
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
        let affected = self
            .actor
            .ask(DbExecute {
                sql: "DELETE FROM native_users WHERE username = ? AND domain = ?".to_string(),
                params: vec![username.into(), domain.into()],
            })
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

/// Generate a random SCRAM salt (16 bytes).
fn generate_scram_salt() -> Vec<u8> {
    rand::random::<[u8; 16]>().to_vec()
}

/// Helper to convert database errors to AuthError.
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
mod tests;
