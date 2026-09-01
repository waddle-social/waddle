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
use thiserror::Error;
use tracing::{debug, instrument, warn};
use uuid::Uuid;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};

use crate::db::actor::{CreateAuthSession, DbActor, DbExecute, DbQueryOne, RowValues};
use crate::db::{row_value, ValueExt};

type HmacSha256 = Hmac<Sha256>;

/// Prefix stored in `sessions.token_hash` for native-SCRAM resume rows
/// instead of a real token hash. The full stored value is
/// `native-resume:<uuid>` — per-row unique (the column carries a unique
/// index) while the `:` guarantees no hex digest can ever match it, so a
/// bearer presentation can never hash-match the row: it backs SM resume
/// identity only and is unusable as an OAUTHBEARER/HTTP credential.
pub const NATIVE_RESUME_TOKEN_SENTINEL: &str = "native-resume";

fn native_resume_token_hash() -> String {
    format!("{NATIVE_RESUME_TOKEN_SENTINEL}:{}", Uuid::new_v4())
}

pub(crate) fn is_native_resume_token_hash(token_hash: &str) -> bool {
    token_hash.starts_with(NATIVE_RESUME_TOKEN_SENTINEL)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Opaque session token and database id.
    pub id: String,
    /// Bare JID principal (e.g. `alice@example.com`). Immutable.
    pub user_jid: String,
    /// Immutable username.
    pub username: String,
    /// Immutable xmpp localpart.
    pub xmpp_localpart: String,
    /// Optional session expiry.
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    /// Non-secret durable identity context used for SM resume. This is not
    /// the bearer token (`id`) and never travels on the XMPP wire.
    pub auth_context_id: Uuid,
    pub auth_context_version: u64,
    pub principal_auth_epoch: u64,
    /// True for rows persisted solely to back the durable SM resume fence
    /// (native SCRAM logins). Such rows are invisible to every bearer path
    /// and are deleted when the connection's resume lineage ends.
    #[serde(default)]
    pub native_resume_only: bool,
}

impl Session {
    pub fn new(user_jid: &str, username: &str, xmpp_localpart: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            user_jid: user_jid.to_string(),
            username: username.to_string(),
            xmpp_localpart: xmpp_localpart.to_string(),
            // 30-day session by default.
            expires_at: Some(Utc::now() + Duration::days(30)),
            created_at: Utc::now(),
            last_used_at: Utc::now(),
            auth_context_id: Uuid::new_v4(),
            auth_context_version: AuthContextVersion::INITIAL.get(),
            principal_auth_epoch: PrincipalAuthEpoch::INITIAL.get(),
            native_resume_only: false,
        }
    }

    /// A session persisted ONLY so the XEP-0198 durable resume fence can
    /// resolve this connection's principal. Its `id` is a row key, not a
    /// credential: `create_session` stores the non-bearer sentinel instead
    /// of a token hash, and every bearer path treats the row as absent.
    pub fn new_native_resume(user_jid: &str, username: &str, xmpp_localpart: &str) -> Self {
        Self {
            native_resume_only: true,
            ..Self::new(user_jid, username, xmpp_localpart)
        }
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at.map(|v| Utc::now() >= v).unwrap_or(false)
    }

    pub fn authenticated_principal_ref(
        &self,
    ) -> Result<AuthenticatedPrincipalRef, AuthenticatedPrincipalRefError> {
        let bare_jid = self
            .user_jid
            .parse()
            .map_err(|_| AuthenticatedPrincipalRefError::InvalidPersistedPrincipalJid)?;
        Ok(AuthenticatedPrincipalRef::new(
            bare_jid,
            AuthContextId::new(self.auth_context_id),
            AuthContextVersion::new(self.auth_context_version),
            PrincipalAuthEpoch::new(self.principal_auth_epoch),
        ))
    }
}

pub struct SessionManager {
    actor: ActorRef<DbActor>,
    hash_key: Option<Vec<u8>>,
}

/// A session row cannot yield a durable principal reference when its persisted
/// JID is invalid.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthenticatedPrincipalRefError {
    #[error("persisted session principal JID is invalid")]
    InvalidPersistedPrincipalJid,
}

/// Closed result of resolving a durable principal reference against the live
/// sessions table. Storage failures remain distinct from invalid references so
/// callers can fail closed without treating an outage as a revocation.
#[derive(Debug)]
pub enum PrincipalResolution {
    Resolved(Session),
    Missing,
    Mismatched,
    StorageError(AuthError),
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
        let token_hash = if session.native_resume_only {
            native_resume_token_hash()
        } else {
            self.token_hash(&session.id)
        };
        let expires_at = session.expires_at.map(|v| v.to_rfc3339());
        let created_at = session.created_at.to_rfc3339();
        let last_used_at = session.last_used_at.to_rfc3339();

        self.actor
            .ask(CreateAuthSession {
                session_id: session.id.clone(),
                user_jid: session.user_jid.clone(),
                username: session.username.clone(),
                xmpp_localpart: session.xmpp_localpart.clone(),
                token_hash,
                auth_context_id: session.auth_context_id,
                auth_context_version: session.auth_context_version,
                principal_auth_epoch: session.principal_auth_epoch,
                expires_at,
                created_at,
                last_used_at,
            })
            .await
            .map_err(Self::ask_err)?;

        debug!(session_id = %session.id, user_jid = %session.user_jid, "Session created");
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
        let user_jid = row_value(row, 1)
            .and_then(ValueExt::as_string)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get user jid: {}", e)))?;
        let token_hash = row_value(row, 2)
            .and_then(ValueExt::as_string)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get token hash: {}", e)))?;

        let native_resume_only = is_native_resume_token_hash(&token_hash);
        if !native_resume_only && token_hash != self.token_hash(&id) {
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
        let auth_context_id = row_value(row, 8)
            .and_then(ValueExt::as_optional_string)
            .map_err(|e| AuthError::DatabaseError(format!("Failed to get auth context id: {e}")))?
            .ok_or_else(|| AuthError::SessionNotFound(id.clone()))?
            .parse()
            .map_err(|e| AuthError::DatabaseError(format!("invalid auth context id: {e}")))?;
        let auth_context_version = integer_column(row, 9, "auth context version")?;
        let principal_auth_epoch = integer_column(row, 10, "principal auth epoch")?;

        Ok(Session {
            id,
            user_jid,
            username,
            xmpp_localpart,
            expires_at,
            created_at,
            last_used_at,
            auth_context_id,
            auth_context_version,
            principal_auth_epoch,
            native_resume_only,
        })
    }

    #[instrument(skip(self))]
    pub async fn get_session(&self, session_id: &str) -> Result<Option<Session>, AuthError> {
        let sql = r#"
            SELECT s.id, s.user_jid, s.token_hash, s.expires_at, s.created_at, s.last_used_at,
                   u.username, u.xmpp_localpart, s.auth_context_id, s.auth_context_version,
                   s.principal_auth_epoch
            FROM sessions s
            JOIN users u ON u.jid = s.user_jid
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
            // Native-resume rows exist only for the SM resume fence; every
            // caller of get_session treats Ok(Some) as bearer-backed, so
            // they are indistinguishable from absent here.
            Some(values) => match self.values_to_session(&values) {
                Ok(session) => Ok((!session.native_resume_only).then_some(session)),
                Err(AuthError::SessionNotFound(_)) => Ok(None),
                Err(error) => Err(error),
            },
            None => Ok(None),
        }
    }

    /// Resolve a non-secret SM principal reference from database authority.
    ///
    /// The reference contains no bearer proof. Its context UUID, exact
    /// version/epoch, bare JID, and expiry must still match the live row.
    #[instrument(skip(self, principal))]
    pub async fn resolve_principal(
        &self,
        principal: &AuthenticatedPrincipalRef,
    ) -> PrincipalResolution {
        let sql = r#"
            SELECT s.id, s.user_jid, s.token_hash, s.expires_at, s.created_at, s.last_used_at,
                   u.username, u.xmpp_localpart, s.auth_context_id, s.auth_context_version,
                   s.principal_auth_epoch
            FROM sessions s
            JOIN users u ON u.jid = s.user_jid
            WHERE s.auth_context_id = ?
            LIMIT 1
        "#
        .to_string();
        let row: Option<RowValues> = match self
            .actor
            .ask(DbQueryOne {
                sql,
                params: vec![crate::db::Value::from(
                    principal.auth_context_id().as_uuid().to_string(),
                )],
            })
            .await
        {
            Ok(row) => row,
            Err(error) => return PrincipalResolution::StorageError(Self::ask_err(error)),
        };

        let Some(values) = row else {
            return PrincipalResolution::Missing;
        };
        let session = match self.values_to_session(&values) {
            Ok(session) => session,
            Err(error) => return PrincipalResolution::StorageError(error),
        };
        let matches = session.auth_context_id == principal.auth_context_id().as_uuid()
            && session.auth_context_version == principal.auth_context_version().get()
            && session.principal_auth_epoch == principal.auth_epoch().get()
            && session.user_jid == principal.bare_jid().to_string();

        if !matches {
            return PrincipalResolution::Mismatched;
        }
        if session.is_expired() {
            return PrincipalResolution::Missing;
        }
        PrincipalResolution::Resolved(session)
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

fn integer_column(row: &[crate::db::Value], index: usize, name: &str) -> Result<u64, AuthError> {
    match row_value(row, index).map_err(|error| AuthError::DatabaseError(error.to_string()))? {
        crate::db::Value::Integer(value) => (*value)
            .try_into()
            .map_err(|_| AuthError::DatabaseError(format!("invalid {name}: {value}"))),
        value => Err(AuthError::DatabaseError(format!(
            "invalid {name} value: {value:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{PrincipalResolution, Session, SessionManager};
    use crate::db::{actor::DbActor, actor::DbExecute, Database, MigrationRunner, Value};
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

        let first = Session::new("alice@example.com", "alice", "alice");
        manager.create_session(&first).await.expect("first session");

        let second = Session::new("alice@example.com", "alice", "alice");
        manager
            .create_session(&second)
            .await
            .expect("second session for existing user");

        let loaded = manager
            .get_session(&second.id)
            .await
            .expect("load session")
            .expect("session exists");

        assert_eq!(loaded.user_jid, "alice@example.com");
        assert_eq!(loaded.username, "alice");
        assert_eq!(loaded.xmpp_localpart, "alice");
        assert_eq!(loaded.auth_context_id, second.auth_context_id);
        assert_eq!(loaded.auth_context_version, 1);
        assert_eq!(loaded.principal_auth_epoch, 1);
    }

    #[tokio::test]
    async fn null_auth_context_row_is_treated_as_absent() {
        let db = Database::in_memory("test-auth-session-legacy-context")
            .await
            .expect("in-memory database");
        let conn = db.guard().await.expect("database guard");
        // V0012 makes NULL impossible in the live schema, so this test
        // exercises the row-decoding guard directly with a handcrafted
        // pre-constraint shape.
        conn.execute_batch(
            r#"
            CREATE TABLE users (
                jid TEXT PRIMARY KEY,
                username TEXT NOT NULL,
                xmpp_localpart TEXT NOT NULL
            );
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                user_jid TEXT NOT NULL,
                token_hash TEXT NOT NULL UNIQUE,
                expires_at TEXT,
                created_at TEXT NOT NULL,
                last_used_at TEXT NOT NULL,
                auth_context_id TEXT,
                auth_context_version INTEGER NOT NULL DEFAULT 1,
                principal_auth_epoch INTEGER NOT NULL DEFAULT 1
            );
            "#,
        )
        .await
        .expect("create pre-constraint schema");
        conn.execute(
            "INSERT INTO users (jid, username, xmpp_localpart) VALUES (?, ?, ?)",
            crate::db_params!["alice@example.com", "alice", "alice"],
        )
        .await
        .expect("insert user");
        let actor = DbActor::spawn(DbActor::new(db.clone()));
        let manager = SessionManager::new(actor, Some(b"test-session-key"));
        let session = Session::new("alice@example.com", "alice", "alice");
        conn.execute(
            r#"
            INSERT INTO sessions (
                id, user_jid, token_hash, expires_at, created_at, last_used_at,
                auth_context_id, auth_context_version, principal_auth_epoch
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                session.id.clone(),
                session.user_jid.clone(),
                manager.token_hash(&session.id),
                session.expires_at.map(|value| value.to_rfc3339()),
                session.created_at.to_rfc3339(),
                session.last_used_at.to_rfc3339(),
                Value::NullText,
                session.auth_context_version,
                session.principal_auth_epoch,
            ],
        )
        .await
        .expect("insert pre-constraint session");
        drop(conn);

        assert!(manager
            .get_session(&session.id)
            .await
            .expect("load invalid row")
            .is_none());
        assert!(matches!(
            manager.validate_session(&session.id).await,
            Err(super::AuthError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn principal_resolution_distinguishes_resolved_missing_mismatched_and_storage_errors() {
        let db = Database::in_memory("test-auth-context-resolution")
            .await
            .expect("in-memory database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrations");
        let actor = DbActor::spawn(DbActor::new(db));
        let manager = SessionManager::new(actor.clone(), Some(b"test-key"));
        let session = Session::new("alice@example.com", "alice", "alice");
        let principal = session
            .authenticated_principal_ref()
            .expect("typed principal");
        manager
            .create_session(&session)
            .await
            .expect("create session");

        match manager.resolve_principal(&principal).await {
            PrincipalResolution::Resolved(resolved) => assert_eq!(resolved.id, session.id),
            outcome => panic!("expected resolved principal, got {outcome:?}"),
        }

        actor
            .ask(DbExecute {
                sql: "UPDATE sessions SET auth_context_version = ? WHERE id = ?".to_string(),
                params: vec![Value::from(2_i64), Value::from(session.id.clone())],
            })
            .await
            .expect("change auth-context version");
        assert!(matches!(
            manager.resolve_principal(&principal).await,
            PrincipalResolution::Mismatched
        ));

        actor
            .ask(DbExecute {
                sql: "UPDATE sessions SET auth_context_version = ?, principal_auth_epoch = ? \
                      WHERE id = ?"
                    .to_string(),
                params: vec![
                    Value::from(1_i64),
                    Value::from(2_i64),
                    Value::from(session.id.clone()),
                ],
            })
            .await
            .expect("revoke principal auth epoch");
        assert!(matches!(
            manager.resolve_principal(&principal).await,
            PrincipalResolution::Mismatched
        ));

        actor
            .ask(DbExecute {
                sql: "UPDATE sessions SET principal_auth_epoch = ?, expires_at = ? WHERE id = ?"
                    .to_string(),
                params: vec![
                    Value::from(1_i64),
                    Value::from((chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339()),
                    Value::from(session.id.clone()),
                ],
            })
            .await
            .expect("expire session");
        assert!(matches!(
            manager.resolve_principal(&principal).await,
            PrincipalResolution::Missing
        ));

        manager
            .delete_session(&session.id)
            .await
            .expect("delete session");
        assert!(matches!(
            manager.resolve_principal(&principal).await,
            PrincipalResolution::Missing
        ));

        actor.kill();
        assert!(matches!(
            manager.resolve_principal(&principal).await,
            PrincipalResolution::StorageError(_)
        ));
    }

    #[tokio::test]
    async fn native_resume_row_is_bearer_inert_but_resolves_as_principal() {
        let db = Database::in_memory("test-native-resume-row")
            .await
            .expect("in-memory database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrations");
        let actor = DbActor::spawn(DbActor::new(db));
        let manager = SessionManager::new(actor.clone(), Some(b"test-key"));
        let session = Session::new_native_resume("alice@example.com", "alice", "alice");
        let principal = session
            .authenticated_principal_ref()
            .expect("typed principal");
        manager
            .create_session(&session)
            .await
            .expect("create native-resume session");

        // The raw row id must NOT function as a bearer credential: both the
        // direct lookup and token validation treat the row as absent.
        assert!(manager
            .get_session(&session.id)
            .await
            .expect("bearer lookup succeeds structurally")
            .is_none());
        assert!(matches!(
            manager.validate_session(&session.id).await,
            Err(super::AuthError::SessionNotFound(_))
        ));

        // The SM resume fence still resolves it as a durable principal.
        match manager.resolve_principal(&principal).await {
            PrincipalResolution::Resolved(resolved) => {
                assert!(resolved.native_resume_only);
                assert_eq!(resolved.id, session.id);
            }
            outcome => panic!("expected resolved native principal, got {outcome:?}"),
        }
    }
}
