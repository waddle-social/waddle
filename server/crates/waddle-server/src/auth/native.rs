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

use super::directory::canonical_account_jid;
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
        let jid = canonical_account_jid(&request.username, &request.domain).ok_or_else(|| {
            AuthError::InvalidUsername("Username is not a valid JID localpart".to_string())
        })?;

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
                    INSERT INTO native_users (username, domain, jid_key, password_hash, salt, iterations, stored_key, server_key, email)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    RETURNING id
                "#
                .to_string(),
                params: vec![
                    request.username.as_str().into(),
                    request.domain.as_str().into(),
                    jid.as_str().into(),
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

    /// Check if an account holds the JID `username@domain`, in whatever case
    /// or Unicode form it was registered. A row a node predating the lookup
    /// keys wrote counts before the backfill keys it, so registering a
    /// case variant of its name cannot take its JID meanwhile.
    pub async fn user_exists(&self, username: &str, domain: &str) -> Result<bool, AuthError> {
        let Some(jid) = canonical_account_jid(username, domain) else {
            return Ok(false);
        };
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: "SELECT 1 FROM native_users WHERE jid_key = ?".to_string(),
                params: vec![jid.as_str().into()],
            })
            .await
            .map_err(db_err)?;

        Ok(row.is_some() || !self.unkeyed_ids(&jid).await?.is_empty())
    }

    /// Get SCRAM credentials for a user.
    pub async fn get_scram_credentials(
        &self,
        username: &str,
        domain: &str,
    ) -> Result<Option<ScramCredentials>, AuthError> {
        let Some(jid) = canonical_account_jid(username, domain) else {
            return Ok(None);
        };
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: r#"
                    SELECT salt, iterations, stored_key, server_key
                    FROM native_users
                    WHERE jid_key = ?
                "#
                .to_string(),
                params: vec![jid.as_str().into()],
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

        let Some(jid) = canonical_account_jid(username, domain) else {
            return Ok(false);
        };
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: "SELECT password_hash FROM native_users WHERE jid_key = ?".to_string(),
                params: vec![jid.as_str().into()],
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
                    WHERE jid_key = ?
                "#
                .to_string(),
                params: vec![
                    password_hash.into(),
                    scram_salt_b64.into(),
                    stored_key.into(),
                    server_key.into(),
                    canonical_account_jid(username, domain)
                        .as_ref()
                        .map(|jid| jid.as_str())
                        .into(),
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

    /// Delete the native account holding the JID `username@domain`, and any
    /// row naming that JID that was written without its lookup key.
    pub async fn delete_user(&self, username: &str, domain: &str) -> Result<bool, AuthError> {
        let Some(jid) = canonical_account_jid(username, domain) else {
            return Ok(false);
        };
        let mut affected = 0;
        for id in self.unkeyed_ids(&jid).await? {
            affected += self
                .delete_rows("DELETE FROM native_users WHERE id = ?", id)
                .await?;
        }
        affected += self
            .delete_rows("DELETE FROM native_users WHERE jid_key = ?", jid.as_str())
            .await?;

        if affected > 0 {
            debug!(username = %username, domain = %domain, "Native user deleted");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Rows naming `jid` that were written without a lookup key: before the
    /// keys existed, or by a node predating them during a rolling upgrade,
    /// until the periodic backfill keys them. Usually none, so the names are
    /// compared in Rust, the only place their canonical form is computed.
    async fn unkeyed_ids(&self, jid: &jid::BareJid) -> Result<Vec<i64>, AuthError> {
        let rows = self
            .actor
            .ask(DbQuery {
                sql: "SELECT id, username, domain FROM native_users WHERE jid_key IS NULL"
                    .to_string(),
                params: vec![],
            })
            .await
            .map_err(db_err)?;
        let mut ids = Vec::new();
        for row in rows {
            let crate::db::Value::Integer(id) = *row_value(&row, 0).map_err(db_err)? else {
                return Err(AuthError::DatabaseError(
                    "invalid native user id".to_string(),
                ));
            };
            let username = row_value(&row, 1)
                .and_then(ValueExt::as_string)
                .map_err(db_err)?;
            let domain = row_value(&row, 2)
                .and_then(ValueExt::as_string)
                .map_err(db_err)?;
            if canonical_account_jid(&username, &domain).as_ref() == Some(jid) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    async fn delete_rows(
        &self,
        sql: &str,
        param: impl Into<crate::db::Value>,
    ) -> Result<u64, AuthError> {
        self.actor
            .ask(DbExecute {
                sql: sql.to_string(),
                params: vec![param.into()],
            })
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to delete user: {}", e)))
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
