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

use crate::db::actor::{DbActor, DbQuery, DbQueryOne};
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

/// XEP-0055 directory entries from both account stores. Prefer OIDC profile
/// data for a shared JID, and rank exact identities before the result limit.
pub(crate) async fn search_local_accounts(
    actor: &ActorRef<DbActor>,
    domain: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<waddle_xmpp::UserDirectoryEntry>, AuthError> {
    let query = query.trim();
    let address = query
        .parse::<jid::BareJid>()
        .ok()
        .filter(|address| address.domain().as_str() == domain);
    let query = address
        .as_ref()
        .and_then(|address| address.node())
        .map_or(query, |node| node.as_str())
        .to_lowercase();
    let pattern = format!("%{}%", escape_like_pattern(&query));
    let rows = actor
        .ask(DbQuery {
            sql: r#"
                WITH accounts AS (
                    SELECT username, xmpp_localpart, display_name, avatar_url, 0 AS source
                    FROM users
                    UNION ALL
                    SELECT username, username, NULL, NULL, 1
                    FROM native_users WHERE domain = ?
                ), ranked AS (
                    SELECT *, ROW_NUMBER() OVER (
                        PARTITION BY LOWER(xmpp_localpart) ORDER BY source, username
                    ) AS position
                    FROM accounts
                )
                SELECT username, xmpp_localpart, display_name, avatar_url
                FROM ranked
                WHERE position = 1 AND (
                    LOWER(username) LIKE ? ESCAPE '\'
                    OR LOWER(xmpp_localpart) LIKE ? ESCAPE '\'
                    OR LOWER(display_name) LIKE ? ESCAPE '\'
                )
                ORDER BY CASE
                    WHEN LOWER(xmpp_localpart) = ? THEN 0
                    WHEN LOWER(username) = ? THEN 1
                    ELSE 2
                END, username, xmpp_localpart
                LIMIT ?
            "#
            .to_string(),
            params: vec![
                domain.into(),
                pattern.as_str().into(),
                pattern.as_str().into(),
                pattern.as_str().into(),
                query.as_str().into(),
                query.as_str().into(),
                i64::try_from(limit).unwrap_or(i64::MAX).into(),
            ],
        })
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;

    let mut entries = Vec::with_capacity(rows.len());
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        let username = row_value(&row, 0)
            .and_then(ValueExt::as_string)
            .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
        let localpart = row_value(&row, 1)
            .and_then(ValueExt::as_string)
            .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
        // Parse the stored localpart as a JID; username slug generation would
        // change valid native names such as `alice+work` into another account.
        let jid = jid::BareJid::new(&format!("{localpart}@{domain}"))
            .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
        if !seen.insert(jid.clone()) {
            continue;
        }
        let display_name = row_value(&row, 2)
            .and_then(ValueExt::as_optional_string)
            .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
        let avatar_url = row_value(&row, 3)
            .and_then(ValueExt::as_optional_string)
            .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
        entries.push(waddle_xmpp::UserDirectoryEntry {
            jid,
            username,
            display_name,
            avatar_url,
        });
    }
    Ok(entries)
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}
