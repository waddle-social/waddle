//! XMPP OAuth authorization grants (XEP-0493), listed and revoked through
//! XEP-0494.
//!
//! A grant is the owner's approval of one client. The client's bearer
//! credential is a session row (see [`super::session`]); the grant row
//! (migration 2025) adds what the owner needs to manage it: an opaque grant
//! id that is not the credential, the scope, and when it was made. Revoking
//! deletes the session, which deletes the grant with it, so the credential
//! stops authenticating and cannot resume a stream-management session.

use chrono::{DateTime, TimeZone, Utc};
use kameo::actor::ActorRef;
use uuid::Uuid;
use waddle_xmpp::xep::xep0493::{parse_scope_value, scope_value, OAuthScope};

use super::session::SessionManager;
use super::AuthError;
use crate::db::actor::{DbActor, DbExecute, DbQuery, DbQueryOne};
use crate::db::{row_value, Value, ValueExt};

/// One grant as its owner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthGrant {
    /// Opaque identifier; the XEP-0494 client id.
    pub grant_id: String,
    pub scopes: Vec<OAuthScope>,
    /// When the owner's approval was exchanged for a credential.
    pub created_at: DateTime<Utc>,
    /// When the credential last authenticated, if ever.
    pub last_used_at: Option<DateTime<Utc>>,
    /// Whether a stream-management session of the credential is attached or
    /// can still be resumed.
    pub connected: bool,
}

/// Storage of XMPP OAuth grants.
#[derive(Clone)]
pub struct OAuthGrantStore {
    actor: ActorRef<DbActor>,
}

fn db_error(error: impl std::fmt::Display) -> AuthError {
    AuthError::DatabaseError(error.to_string())
}

fn integer(row: &[Value], index: usize) -> Result<i64, AuthError> {
    match row_value(row, index).map_err(db_error)? {
        Value::Integer(value) => Ok(*value),
        other => Err(db_error(format!("expected integer, got {other:?}"))),
    }
}

fn text(row: &[Value], index: usize) -> Result<String, AuthError> {
    row_value(row, index)
        .and_then(ValueExt::as_string)
        .map_err(db_error)
}

impl OAuthGrantStore {
    pub fn new(sessions: &SessionManager) -> Self {
        Self {
            actor: sessions.actor_ref(),
        }
    }

    /// Record the grant behind `session_id` and return its id.
    pub async fn create(
        &self,
        session_id: &str,
        user_jid: &str,
        scopes: &[OAuthScope],
    ) -> Result<String, AuthError> {
        let grant_id = Uuid::new_v4().simple().to_string();
        self.actor
            .ask(DbExecute {
                sql: "INSERT INTO xmpp_oauth_grants \
                      (grant_id, session_id, user_jid, scope, created_at_ms) \
                      VALUES (?, ?, ?, ?, ?)"
                    .to_string(),
                params: vec![
                    Value::from(grant_id.as_str()),
                    Value::from(session_id),
                    Value::from(user_jid),
                    Value::from(scope_value(scopes)),
                    Value::from(Utc::now().timestamp_millis()),
                ],
            })
            .await
            .map_err(db_error)?;
        Ok(grant_id)
    }

    /// The live grants of `user_jid`, oldest first. A grant whose credential
    /// is expired is not listed: it grants nothing.
    pub async fn list(&self, user_jid: &str) -> Result<Vec<OAuthGrant>, AuthError> {
        let rows = self
            .actor
            .ask(DbQuery {
                sql: "SELECT g.grant_id, g.scope, g.created_at_ms, s.last_used_at, s.expires_at, \
                      CASE WHEN EXISTS ( \
                          SELECT 1 FROM sm_durable_sessions d \
                          WHERE d.auth_context_id = s.auth_context_id AND d.state IN (1, 2, 3) \
                      ) THEN 1 ELSE 0 END \
                      FROM xmpp_oauth_grants g \
                      JOIN sessions s ON s.id = g.session_id \
                      WHERE g.user_jid = ? \
                      ORDER BY g.created_at_ms, g.grant_id"
                    .to_string(),
                params: vec![Value::from(user_jid)],
            })
            .await
            .map_err(db_error)?;

        let now = Utc::now();
        let mut grants = Vec::new();
        for row in rows {
            let expires_at = row_value(&row, 4)
                .and_then(ValueExt::as_optional_string)
                .map_err(db_error)?
                .map(|value| SessionManager::parse_ts(&value))
                .transpose()?;
            if expires_at.is_some_and(|expires_at| now >= expires_at) {
                continue;
            }
            let created_at = Utc
                .timestamp_millis_opt(integer(&row, 2)?)
                .single()
                .ok_or_else(|| db_error("invalid grant creation time"))?;
            let last_used = SessionManager::parse_ts(&text(&row, 3)?)?;
            let scopes = parse_scope_value(&text(&row, 1)?)
                .map_err(|_| db_error("stored grant scope is not supported"))?;
            grants.push(OAuthGrant {
                grant_id: text(&row, 0)?,
                scopes,
                created_at,
                // The grant's creation time has millisecond resolution, the
                // session's last use finer: compare at the coarser one.
                last_used_at: (last_used.timestamp_millis() > created_at.timestamp_millis())
                    .then_some(last_used),
                connected: integer(&row, 5)? != 0,
            });
        }
        Ok(grants)
    }

    /// Revoke `grant_id` if `user_jid` owns it. Returns whether a grant was
    /// revoked.
    pub async fn revoke(&self, user_jid: &str, grant_id: &str) -> Result<bool, AuthError> {
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: "SELECT session_id FROM xmpp_oauth_grants WHERE grant_id = ? AND user_jid = ?"
                    .to_string(),
                params: vec![Value::from(grant_id), Value::from(user_jid)],
            })
            .await
            .map_err(db_error)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let session_id = text(&row, 0)?;
        // The grant row goes with its session (ON DELETE CASCADE).
        self.actor
            .ask(DbExecute {
                sql: "DELETE FROM sessions WHERE id = ?".to_string(),
                params: vec![Value::from(session_id)],
            })
            .await
            .map_err(db_error)?;
        Ok(true)
    }

    /// The scopes of the grant behind a bearer session, or `None` when the
    /// session is not an OAuth grant (a login the owner made directly).
    pub async fn scopes_of_session(
        &self,
        session_id: &str,
    ) -> Result<Option<Vec<OAuthScope>>, AuthError> {
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: "SELECT scope FROM xmpp_oauth_grants WHERE session_id = ?".to_string(),
                params: vec![Value::from(session_id)],
            })
            .await
            .map_err(db_error)?;
        row.map(|row| {
            parse_scope_value(&text(&row, 0)?)
                .map_err(|_| db_error("stored grant scope is not supported"))
        })
        .transpose()
    }
}
