//! Local session management for provider-authenticated users.
//!
//! All database access is routed through a `DbActor` to serialise operations
//! and avoid SQLite write-lock contention.

use super::AuthError;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use kameo::actor::ActorRef;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use crate::db::actor::{CreateAuthSession, DbActor, DbExecute, DbQueryOne, RowValues};
use crate::db::{row_value, ValueExt};

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
    actor: ActorRef<DbActor>,
    hash_key: Option<Vec<u8>>,
}

impl SessionManager {
    pub fn new(actor: ActorRef<DbActor>, hash_key: Option<&[u8]>) -> Self {
        Self {
            actor,
            hash_key: hash_key.map(|k| k.to_vec()),
        }
    }

    pub fn actor_ref(&self) -> ActorRef<DbActor> {
        self.actor.clone()
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

    /// Convert an actor ask error into an AuthError.
    fn ask_err(e: impl std::fmt::Display) -> AuthError {
        AuthError::DatabaseError(e.to_string())
    }

    #[instrument(skip(self, session))]
    pub async fn create_session(&self, session: &Session) -> Result<(), AuthError> {
        let token_hash = self.token_hash(&session.id);
        let expires_at = session.expires_at.map(|v| v.to_rfc3339());
        let created_at = session.created_at.to_rfc3339();
        let last_used_at = session.last_used_at.to_rfc3339();

        self.actor
            .ask(CreateAuthSession {
                session_id: session.id.clone(),
                user_id: session.user_id.clone(),
                username: session.username.clone(),
                xmpp_localpart: session.xmpp_localpart.clone(),
                token_hash,
                expires_at,
                created_at,
                last_used_at,
            })
            .await
            .map_err(Self::ask_err)?;

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

    fn values_to_session(&self, row: &[crate::db::Value]) -> Result<Session, AuthError> {
        let id = row_value(row, 0)
            .and_then(ValueExt::as_string)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get session id: {}", e)))?;
        let user_id = row_value(row, 1)
            .and_then(ValueExt::as_string)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get user id: {}", e)))?;
        let token_hash = row_value(row, 2)
            .and_then(ValueExt::as_string)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get token hash: {}", e)))?;

        if token_hash != self.token_hash(&id) {
            return Err(AuthError::SessionNotFound(id));
        }

        let expires_at = row_value(row, 3)
            .and_then(ValueExt::as_optional_string)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get expires_at: {}", e)))?
            .map(|v| Self::parse_ts(&v))
            .transpose()?;
        let created_at = Self::parse_ts(
            &row_value(row, 4)
                .and_then(ValueExt::as_string)
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to get created_at: {}", e))
                })?,
        )?;
        let last_used_at = Self::parse_ts(
            &row_value(row, 5)
                .and_then(ValueExt::as_string)
                .map_err(|e| {
                    AuthError::DatabaseError(format!("Failed to get last_used_at: {}", e))
                })?,
        )?;

        let username = row_value(row, 6)
            .and_then(ValueExt::as_string)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get username: {}", e)))?;
        let xmpp_localpart = row_value(row, 7)
            .and_then(ValueExt::as_string)
            .map_err(|e| {
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
        let sql = r#"
            SELECT s.id, s.user_id, s.token_hash, s.expires_at, s.created_at, s.last_used_at,
                   u.username, u.xmpp_localpart
            FROM sessions s
            JOIN users u ON u.id = s.user_id
            WHERE s.id = ?
            LIMIT 1
        "#
        .to_string();

        let row: Option<RowValues> = self
            .actor
            .ask(DbQueryOne {
                sql,
                params: vec![crate::db::Value::from(session_id)],
            })
            .await
            .map_err(Self::ask_err)?;

        match row {
            Some(values) => Ok(Some(self.values_to_session(&values)?)),
            None => Ok(None),
        }
    }

    #[instrument(skip(self))]
    pub async fn touch_session(&self, session_id: &str) -> Result<(), AuthError> {
        let now = Utc::now().to_rfc3339();

        self.actor
            .ask(DbExecute {
                sql: "UPDATE sessions SET last_used_at = ? WHERE id = ?".to_string(),
                params: vec![
                    crate::db::Value::from(now),
                    crate::db::Value::from(session_id),
                ],
            })
            .await
            .map_err(Self::ask_err)?;
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn delete_session(&self, session_id: &str) -> Result<(), AuthError> {
        self.actor
            .ask(DbExecute {
                sql: "DELETE FROM sessions WHERE id = ?".to_string(),
                params: vec![crate::db::Value::from(session_id)],
            })
            .await
            .map_err(Self::ask_err)?;
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
}

#[cfg(test)]
mod tests {
    use super::{Session, SessionManager};
    use crate::db::{actor::DbActor, Database, MigrationRunner};
    use kameo::actor::Spawn;

    #[tokio::test]
    async fn create_session_creates_missing_user_and_allows_existing_user() {
        let db = Database::in_memory("test-auth-session")
            .await
            .expect("in-memory database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrations");
        let actor = DbActor::spawn(DbActor::new(db.clone()));
        let manager = SessionManager::new(actor, Some(b"test-session-key"));

        let first = Session::new("user-1", "alice", "alice");
        manager.create_session(&first).await.expect("first session");

        let second = Session::new("user-1", "alice", "alice");
        manager
            .create_session(&second)
            .await
            .expect("second session for existing user");

        let loaded = manager
            .get_session(&second.id)
            .await
            .expect("load session")
            .expect("session exists");

        assert_eq!(loaded.user_id, "user-1");
        assert_eq!(loaded.username, "alice");
        assert_eq!(loaded.xmpp_localpart, "alice");
    }
}
