//! `user_avatar_source` provenance — the table that distinguishes
//! OIDC-managed avatars (which the bridge owns and may overwrite /
//! remove on re-login) from user-self-published avatars (which it
//! must NOT touch).
//!
//! Two surfaces:
//!
//! - [`read_avatar_source`] — the OIDC publish chain consults this
//!   on `RemoveIfOidcOwned` and suppresses the empty-`<metadata/>`
//!   publish when the row says `'user'`.
//! - [`record_self_published`] / [`record_user_retracted`] — the
//!   WebSocket PEP-publish handler updates this after a user
//!   wire-publish to their own avatar / vCard4 photo node.

use std::sync::Arc;

use jid::BareJid;
use kameo::actor::ActorRef;
use thiserror::Error;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::warn;

use crate::db::actor::{DbActor, DbExecute, DbQueryOne};
use crate::server::routes::websocket::WebSocketState;

/// Outcome of the user-managed avatar guard query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarSource {
    /// `source = 'oidc'` (the default for fresh OIDC publishes). The
    /// OIDC bridge owns the avatar.
    Oidc,
    /// `source = 'user'`. The user has self-published via wire
    /// XEP-0084 and the bridge MUST NOT overwrite or remove it.
    User,
    /// No row matched. Means the user has neither been touched by
    /// the OIDC bridge nor wire-published an avatar yet — the
    /// guard's downstream idempotence check (was there ever an
    /// avatar?) handles this safely.
    Unknown,
}

impl AvatarSource {
    fn parse(raw: &str) -> Self {
        match raw {
            "user" => AvatarSource::User,
            "oidc" => AvatarSource::Oidc,
            _ => AvatarSource::Unknown,
        }
    }
}

/// Typed error from the avatar-source storage helpers. The single
/// variant wraps the kameo actor error string (which already
/// Display-formats the inner DB error) — kameo's `ask` reply is
/// flattened, so a deeper typed wrapper isn't reachable here without
/// re-introducing string-formatting at the actor seam.
#[derive(Debug, Error)]
pub enum AvatarSourceStorageError {
    #[error("avatar_source storage failure: {0}")]
    Storage(String),
}

/// Read the provenance flag for the user owning `jid`. Returns
/// `Unknown` when the row is missing.
pub async fn read_avatar_source(
    db_actor: &ActorRef<DbActor>,
    jid: &BareJid,
) -> Result<AvatarSource, AvatarSourceStorageError> {
    let localpart = jid
        .node()
        .map(|n| n.as_str().to_string())
        .unwrap_or_default();
    let row = db_actor
        .ask(DbQueryOne {
            sql: "SELECT source FROM user_avatar_source WHERE xmpp_localpart = ? LIMIT 1"
                .to_string(),
            params: vec![localpart.into()],
        })
        .await
        .map_err(|e| AvatarSourceStorageError::Storage(e.to_string()))?;
    let Some(row) = row else {
        return Ok(AvatarSource::Unknown);
    };
    let raw: Option<String> = row.first().and_then(|v| match v {
        crate::db::Value::Text(s) => Some(s.clone()),
        _ => None,
    });
    Ok(raw
        .as_deref()
        .map(AvatarSource::parse)
        .unwrap_or(AvatarSource::Unknown))
}

async fn upsert_source(
    db_actor: &ActorRef<DbActor>,
    localpart: &str,
    source: &str,
) -> Result<(), AvatarSourceStorageError> {
    let now = chrono::Utc::now().to_rfc3339();
    db_actor
        .ask(DbExecute {
            sql: r#"
                INSERT INTO user_avatar_source (xmpp_localpart, source, updated_at)
                VALUES (?, ?, ?)
                ON CONFLICT(xmpp_localpart) DO UPDATE
                  SET source = excluded.source, updated_at = excluded.updated_at
            "#
            .to_string(),
            params: vec![localpart.into(), source.into(), now.into()],
        })
        .await
        .map_err(|e| AvatarSourceStorageError::Storage(e.to_string()))?;
    Ok(())
}

/// Acquire the per-(`BareJid`) avatar-source mutex. Both the OIDC
/// publish chain and the wire avatar-publish hook take this lock;
/// holding it across read+write closes the TOCTOU race where OIDC
/// could read `'oidc'` between a user's wire publish and the
/// downstream `record_self_published` flip and then wipe the
/// just-set avatar.
///
/// Returns an `OwnedMutexGuard` so the caller can hold it across
/// further async operations without lifetime gymnastics.
pub async fn acquire_per_jid_lock(state: &WebSocketState, jid: &BareJid) -> OwnedMutexGuard<()> {
    let mutex = state
        .deps
        .protocol
        .avatar_source_locks
        .entry(jid.clone())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    mutex.lock_owned().await
}

/// Mark `jid`'s avatar as user-self-published. Idempotent — repeated
/// calls just re-confirm `'user'`. Logs and swallows errors (the
/// publish itself already succeeded; failing the IQ because we
/// couldn't update a provenance flag would be worse).
pub async fn record_self_published(db_actor: &ActorRef<DbActor>, jid: &BareJid) {
    let Some(localpart) = jid.node().map(|n| n.as_str().to_string()) else {
        return;
    };
    if let Err(error) = upsert_source(db_actor, &localpart, "user").await {
        warn!(
            jid = %jid,
            error = %error,
            "Failed to mark avatar_source='user'; OIDC reconcile may still overwrite"
        );
    }
}

/// Mark `jid`'s avatar as OIDC-managed. Called when the user
/// explicitly opts back into OIDC management (e.g. by wire-publishing
/// the XEP-0084 §4.3 empty-`<metadata/>` removal shape themselves —
/// "I'm not picking my own avatar anymore, you can manage it") AND
/// after the OIDC bridge successfully publishes a new avatar (so the
/// flag tracks current ownership intent).
pub async fn record_oidc_managed(db_actor: &ActorRef<DbActor>, jid: &BareJid) {
    let Some(localpart) = jid.node().map(|n| n.as_str().to_string()) else {
        return;
    };
    if let Err(error) = upsert_source(db_actor, &localpart, "oidc").await {
        warn!(
            jid = %jid,
            error = %error,
            "Failed to mark avatar_source='oidc'; provenance state may be stale"
        );
    }
}
