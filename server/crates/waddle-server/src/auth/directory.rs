//! Unified local-account existence across Waddle's two registration paths.
//!
//! Waddle stores local identities in two tables that are populated by
//! independent provisioning flows:
//!
//! - `users` — accounts created through OIDC/web login, keyed by
//!   `xmpp_localpart`. These carry no password material and no `domain`
//!   column; an OIDC account is always local to the server's own domain.
//! - `native_users` — XEP-0077 / SCRAM accounts, keyed by
//!   `(username, domain)`.
//!
//! A JID belongs to a real local account when it is present in *either*
//! table. Callers that must recognise every registered identity — regardless
//! of how it was provisioned — use [`local_account_exists`] rather than
//! [`crate::auth::NativeUserStore::user_exists`], which only sees native
//! accounts and therefore reports every OIDC user as non-existent.
//!
//! The admin Users panel already unions both tables for the same reason (see
//! `admin/users_list.rs`); this is the single-row existence counterpart.

use kameo::actor::ActorRef;

use crate::db::actor::{DbActor, DbQueryOne};
use crate::db::{row_value, ValueExt};

use super::AuthError;

#[cfg(test)]
mod tests;

/// Returns `true` when `localpart@domain` resolves to a registered local
/// account through either the OIDC `users` table or the native `native_users`
/// table.
///
/// `users` rows carry no `domain` column — OIDC accounts are always local to
/// the server's own domain — so they are matched on `xmpp_localpart` alone.
/// Native accounts are matched on `(username, domain)`. Callers are expected
/// to have already constrained `domain` to the local server domain (group-DM
/// validation, for example, rejects non-local members before reaching here).
pub async fn local_account_exists(
    actor: &ActorRef<DbActor>,
    localpart: &str,
    domain: &str,
) -> Result<bool, AuthError> {
    let row = actor
        .ask(DbQueryOne {
            sql: "SELECT 1 FROM users WHERE xmpp_localpart = ? \
                  UNION ALL \
                  SELECT 1 FROM native_users WHERE username = ? AND domain = ? \
                  LIMIT 1"
                .to_string(),
            params: vec![localpart.into(), localpart.into(), domain.into()],
        })
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;

    Ok(row.is_some())
}

/// Resolve a local user's `users.id` — the canonical SpiceDB subject id used
/// throughout Waddle's permission model — from their `xmpp_localpart`.
///
/// Returns `None` when the localpart has no OIDC `users` row. SpiceDB object ids
/// forbid `@`/`.`, so a JID can never be a valid subject; this UUID is the
/// stable handle. Native-only accounts (no `users` row) are not representable as
/// permission subjects and resolve to `None`.
pub async fn resolve_user_id(
    actor: &ActorRef<DbActor>,
    localpart: &str,
) -> Result<Option<String>, AuthError> {
    let row = actor
        .ask(DbQueryOne {
            sql: "SELECT id FROM users WHERE xmpp_localpart = ? LIMIT 1".to_string(),
            params: vec![localpart.into()],
        })
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;

    match row {
        Some(row) => {
            let id = row_value(&row, 0)
                .and_then(ValueExt::as_string)
                .map_err(|error| AuthError::DatabaseError(format!("decode user id: {error}")))?;
            Ok(Some(id))
        }
        None => Ok(None),
    }
}
