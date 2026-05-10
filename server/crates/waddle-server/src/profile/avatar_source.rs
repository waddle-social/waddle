//! `users.avatar_source` provenance — the column that distinguishes
//! OIDC-managed avatars (which the bridge owns and may overwrite /
//! remove on re-login) from user-self-published avatars (which it
//! must NOT touch).
//!
//! Two surfaces:
//!
//! - [`read_avatar_source`] — the OIDC publish chain consults this
//!   on `RemoveIfOidcOwned` and suppresses the empty-`<metadata/>`
//!   publish when the row says `'user'`.
//! - [`record_self_published`] — the WebSocket PEP-publish handler
//!   calls this after a user successfully publishes to their own
//!   `urn:xmpp:avatar:data` / `urn:xmpp:avatar:metadata` node, so
//!   the next OIDC reconcile honors the user's choice.

use jid::BareJid;
use kameo::actor::ActorRef;
use tracing::warn;

use crate::db::actor::{DbActor, DbExecute, DbQueryOne};

/// Outcome of the user-managed avatar guard query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarSource {
    /// `users.avatar_source = 'oidc'` (the default for fresh
    /// provisions). The OIDC bridge owns the avatar.
    Oidc,
    /// `users.avatar_source = 'user'`. The user has self-published
    /// via wire XEP-0084 and the bridge must not overwrite or
    /// remove their picture.
    User,
    /// No row matched — typically means the user hasn't been
    /// provisioned (the bridge's `RemoveIfOidcOwned` treats this as
    /// "no avatar to remove" by virtue of upstream idempotence
    /// checks; downstream is no-op).
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

/// Read the `avatar_source` flag for the user owning `jid`. Returns
/// `Unknown` when the row is missing (fresh user not yet
/// provisioned, or test fixture).
pub async fn read_avatar_source(
    db_actor: &ActorRef<DbActor>,
    jid: &BareJid,
) -> Result<AvatarSource, String> {
    let localpart = jid
        .node()
        .map(|n| n.as_str().to_string())
        .unwrap_or_default();
    let row = db_actor
        .ask(DbQueryOne {
            sql: "SELECT avatar_source FROM users WHERE xmpp_localpart = ? LIMIT 1".to_string(),
            params: vec![localpart.into()],
        })
        .await
        .map_err(|e| format!("avatar_source query actor failure: {e}"))?;
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

/// Mark `jid`'s avatar as user-self-published. Idempotent —
/// repeated calls just re-confirm `'user'`. Logs and swallows
/// errors (the publish itself already succeeded; failing the IQ
/// because we couldn't update a provenance flag would be worse).
///
/// Implementation note — UPSERT semantics: native-auth users (the
/// XEP-0077 / SCRAM path and the test fixed-account fixture) only
/// land in `native_users`, not in `users`. A naive
/// `UPDATE users SET avatar_source='user'` would silently target
/// zero rows for those users and the guard would never fire on
/// their next OIDC reconcile. To make the user-managed guard work
/// uniformly across both auth paths, we upsert: insert a minimal
/// `users` row keyed on the localpart if one isn't there, or flip
/// the column on the existing row. The user just demonstrably
/// owns their PEP avatar, so a corresponding identity row is the
/// right shape.
pub async fn record_self_published(db_actor: &ActorRef<DbActor>, jid: &BareJid) {
    let localpart = match jid.node() {
        Some(n) => n.as_str().to_string(),
        None => return,
    };
    let now = chrono::Utc::now().to_rfc3339();
    let new_id = uuid::Uuid::new_v4().to_string();
    if let Err(error) = db_actor
        .ask(DbExecute {
            sql: r#"
                INSERT INTO users (id, username, xmpp_localpart, created_at, updated_at, avatar_source)
                VALUES (?, ?, ?, ?, ?, 'user')
                ON CONFLICT(xmpp_localpart) DO UPDATE
                  SET avatar_source = 'user', updated_at = excluded.updated_at
            "#
            .to_string(),
            params: vec![
                new_id.into(),
                localpart.clone().into(),
                localpart.into(),
                now.clone().into(),
                now.into(),
            ],
        })
        .await
    {
        warn!(
            jid = %jid,
            error = %error,
            "Failed to mark avatar_source='user'; OIDC reconcile may still overwrite"
        );
    }
}
