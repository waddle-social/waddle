//! Local session management for provider-authenticated users.

use super::AuthError;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use crate::db::Database;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Opaque session token and database id.
    pub id: String,
    /// Internal UUID principal.
    pub user_id: String,
    /// Immutable username.
    pub username: String,
    /// Immutable xmpp localpart.
    pub xmpp_localpart: String,
    /// Optional session expiry.
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
}

impl Session {
    pub fn new(user_id: &str, username: &str, xmpp_localpart: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            user_id: user_id.to_string(),
            username: username.to_string(),
            xmpp_localpart: xmpp_localpart.to_string(),
            // 30-day session by default.
            expires_at: Some(Utc::now() + Duration::days(30)),
            created_at: Utc::now(),
            last_used_at: Utc::now(),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at.map(|v| Utc::now() >= v).unwrap_or(false)
    }
}

pub struct SessionManager {
    db: Arc<Database>,
    hash_key: Option<Vec<u8>>,
}

impl SessionManager {
    pub fn new(db: Arc<Database>, hash_key: Option<&[u8]>) -> Self {
        Self {
            db,
            hash_key: hash_key.map(|k| k.to_vec()),
        }
    }

    fn token_hash(&self, token: &str) -> String {
        match &self.hash_key {
            Some(key) => {
                let mut mac = HmacSha256::new_from_slice(key).expect("HMAC supports any key");
                mac.update(token.as_bytes());
                hex::encode(mac.finalize().into_bytes())
            }
            None => {
                let mut hasher = Sha256::new();
                hasher.update(token.as_bytes());
                hex::encode(hasher.finalize())
            }
        }
    }

    async fn ensure_user_exists(&self, session: &Session) -> Result<(), AuthError> {
        let query = r#"
            INSERT OR IGNORE INTO users (id, username, xmpp_localpart, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?)
        "#;

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(
                query,
                libsql::params![
                    session.user_id.as_str(),
                    session.username.as_str(),
                    session.xmpp_localpart.as_str(),
                    session.created_at.to_rfc3339(),
                    session.created_at.to_rfc3339()
                ],
            )
            .await
            .map_err(|e| {
                AuthError::DatabaseError(format!("Failed to ensure user exists: {}", e))
            })?;
            Ok(())
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect database: {}", e))
            })?;
            conn.execute(
                query,
                libsql::params![
                    session.user_id.as_str(),
                    session.username.as_str(),
                    session.xmpp_localpart.as_str(),
                    session.created_at.to_rfc3339(),
                    session.created_at.to_rfc3339()
                ],
            )
            .await
            .map_err(|e| {
                AuthError::DatabaseError(format!("Failed to ensure user exists: {}", e))
            })?;
            Ok(())
        }
    }

    #[instrument(skip(self, session))]
    pub async fn create_session(&self, session: &Session) -> Result<(), AuthError> {
        self.ensure_user_exists(session).await?;

        let token_hash = self.token_hash(&session.id);
        let expires_at = session.expires_at.map(|v| v.to_rfc3339());

        let query = r#"
            INSERT INTO sessions (id, user_id, token_hash, expires_at, created_at, last_used_at)
            VALUES (?, ?, ?, ?, ?, ?)
        "#;

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(
                query,
                libsql::params![
                    session.id.as_str(),
                    session.user_id.as_str(),
                    token_hash,
                    expires_at,
                    session.created_at.to_rfc3339(),
                    session.last_used_at.to_rfc3339()
                ],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to insert session: {}", e)))?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect database: {}", e))
            })?;
            conn.execute(
                query,
                libsql::params![
                    session.id.as_str(),
                    session.user_id.as_str(),
                    token_hash,
                    expires_at,
                    session.created_at.to_rfc3339(),
                    session.last_used_at.to_rfc3339()
                ],
            )
            .await
            .map_err(|e| AuthError::DatabaseError(format!("Failed to insert session: {}", e)))?;
        }

        debug!(session_id = %session.id, user_id = %session.user_id, "Session created");
        Ok(())
    }

    fn parse_ts(value: &str) -> Result<DateTime<Utc>, AuthError> {
        if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
            return Ok(dt.with_timezone(&Utc));
        }

        if let Ok(naive) = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
            return Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
        }

        Err(AuthError::DatabaseError(format!(
            "failed to parse timestamp: {}",
            value
        )))
    }

    fn row_to_session(&self, row: &libsql::Row) -> Result<Session, AuthError> {
        let id: String = row
            .get(0)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get session id: {}", e)))?;
        let user_id: String = row
            .get(1)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get user id: {}", e)))?;
        let token_hash: String = row
            .get(2)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get token hash: {}", e)))?;

        if token_hash != self.token_hash(&id) {
            return Err(AuthError::SessionNotFound(id));
        }

        let expires_at = row
            .get::<Option<String>>(3)
            .ok()
            .flatten()
            .map(|v| Self::parse_ts(&v))
            .transpose()?;
        let created_at =
            Self::parse_ts(&row.get::<String>(4).map_err(|e| {
                AuthError::DatabaseError(format!("Failed to get created_at: {}", e))
            })?)?;
        let last_used_at = Self::parse_ts(&row.get::<String>(5).map_err(|e| {
            AuthError::DatabaseError(format!("Failed to get last_used_at: {}", e))
        })?)?;

        let username: String = row
            .get(6)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get username: {}", e)))?;
        let xmpp_localpart: String = row.get(7).map_err(|e| {
            AuthError::DatabaseError(format!("Failed to get xmpp_localpart: {}", e))
        })?;

        Ok(Session {
            id,
            user_id,
            username,
            xmpp_localpart,
            expires_at,
            created_at,
            last_used_at,
        })
    }

    #[instrument(skip(self))]
    pub async fn get_session(&self, session_id: &str) -> Result<Option<Session>, AuthError> {
        let query = r#"
            SELECT s.id, s.user_id, s.token_hash, s.expires_at, s.created_at, s.last_used_at,
                   u.username, u.xmpp_localpart
            FROM sessions s
            JOIN users u ON u.id = s.user_id
            WHERE s.id = ?
            LIMIT 1
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
                AuthError::DatabaseError(format!("Failed to connect database: {}", e))
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
            Some(row) => Ok(Some(self.row_to_session(&row)?)),
            None => Ok(None),
        }
    }

    #[instrument(skip(self))]
    pub async fn touch_session(&self, session_id: &str) -> Result<(), AuthError> {
        let now = Utc::now().to_rfc3339();
        let query = "UPDATE sessions SET last_used_at = ? WHERE id = ?";

        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(query, libsql::params![now, session_id])
                .await
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to update session: {}", e))
                })?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect database: {}", e))
            })?;
            conn.execute(query, libsql::params![now, session_id])
                .await
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to update session: {}", e))
                })?;
        }
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn delete_session(&self, session_id: &str) -> Result<(), AuthError> {
        let query = "DELETE FROM sessions WHERE id = ?";
        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(query, libsql::params![session_id])
                .await
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to delete session: {}", e))
                })?;
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect database: {}", e))
            })?;
            conn.execute(query, libsql::params![session_id])
                .await
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to delete session: {}", e))
                })?;
        }
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn validate_session(&self, session_id: &str) -> Result<Session, AuthError> {
        let session = self
            .get_session(session_id)
            .await?
            .ok_or_else(|| AuthError::SessionNotFound(session_id.to_string()))?;

        if session.is_expired() {
            warn!(session_id = %session_id, "Session expired");
            return Err(AuthError::SessionExpired);
        }

        self.touch_session(session_id).await?;
        Ok(session)
    }

    #[allow(dead_code)]
    pub async fn cleanup_expired_sessions(&self) -> Result<usize, AuthError> {
        let now = Utc::now().to_rfc3339();
        let query = "DELETE FROM sessions WHERE expires_at IS NOT NULL AND expires_at < ?";
        let deleted = if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(query, libsql::params![now])
                .await
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed cleanup expired sessions: {}", e))
                })?
        } else {
            let conn = self.db.connect().map_err(|e| {
                AuthError::DatabaseError(format!("Failed to connect database: {}", e))
            })?;
            conn.execute(query, libsql::params![now])
                .await
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed cleanup expired sessions: {}", e))
                })?
        };
        Ok(deleted as usize)
    }
}
