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

use std::borrow::Cow;

use kameo::actor::ActorRef;
use tracing::warn;

use crate::db::actor::{DbActor, DbExecute, DbQuery, DbQueryOne};
use crate::db::{row_value, ValueExt};

use super::AuthError;

#[cfg(test)]
mod tests;

/// The JID library's canonical (nodeprepped) form of an account name: the
/// localpart every session, roster item and stanza carries for it. `None`
/// when the name is no valid localpart. Native usernames keep the case they
/// were registered with, so `Äda` is the account behind `äda@…`.
pub(crate) fn canonical_localpart(name: &str) -> Option<jid::NodePart> {
    jid::NodePart::new(name).ok().map(Cow::into_owned)
}

/// The canonical bare JID of the account `name@domain`.
pub(crate) fn canonical_account_jid(name: &str, domain: &str) -> Option<jid::BareJid> {
    let node = jid::NodePart::new(name).ok()?;
    let domain = jid::DomainPart::new(domain).ok()?;
    Some(jid::BareJid::from_parts(Some(&node), &domain))
}

/// Startup pass before the node serves: give every account row written
/// without one its canonical lookup key. Idempotent.
pub(crate) async fn reconcile_local_accounts(actor: &ActorRef<DbActor>) -> Result<(), AuthError> {
    backfill_account_keys(actor).await
}

// ponytail: one UPDATE per unkeyed row; batch it if a large legacy table makes startup slow.
async fn backfill_account_keys(actor: &ActorRef<DbActor>) -> Result<(), AuthError> {
    let native = query(
        actor,
        "SELECT username, domain FROM native_users WHERE jid_key IS NULL ORDER BY id",
        vec![],
    )
    .await?;
    for row in native {
        let username = text(&row, 0)?;
        let domain = text(&row, 1)?;
        let Some(jid) = canonical_account_jid(&username, &domain) else {
            warn!(%username, %domain, "native account name is no JID; it gets no lookup key");
            continue;
        };
        // Registration compared raw names, so differently cased names may
        // share a JID: the oldest account keeps it.
        let keyed = execute(
            actor,
            "UPDATE native_users SET jid_key = ? \
             WHERE username = ? AND domain = ? AND jid_key IS NULL \
             AND NOT EXISTS (SELECT 1 FROM native_users WHERE jid_key = ?)",
            vec![
                jid.as_str().into(),
                username.as_str().into(),
                domain.as_str().into(),
                jid.as_str().into(),
            ],
        )
        .await?;
        if keyed == 0 {
            warn!(%username, %jid, "native account shares its JID with an older account; it gets no lookup key");
        }
    }

    let users = query(
        actor,
        "SELECT jid, xmpp_localpart FROM users WHERE localpart_key IS NULL",
        vec![],
    )
    .await?;
    for row in users {
        let user_jid = text(&row, 0)?;
        let localpart = text(&row, 1)?;
        let Some(key) = canonical_localpart(&localpart) else {
            warn!(%user_jid, "account localpart is no JID localpart; it gets no lookup key");
            continue;
        };
        execute(
            actor,
            "UPDATE users SET localpart_key = ? WHERE jid = ?",
            vec![key.as_str().into(), user_jid.into()],
        )
        .await?;
    }
    Ok(())
}

async fn query(
    actor: &ActorRef<DbActor>,
    sql: &str,
    params: Vec<crate::db::Value>,
) -> Result<Vec<crate::db::actor::RowValues>, AuthError> {
    actor
        .ask(DbQuery {
            sql: sql.to_string(),
            params,
        })
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))
}

async fn execute(
    actor: &ActorRef<DbActor>,
    sql: &str,
    params: Vec<crate::db::Value>,
) -> Result<u64, AuthError> {
    actor
        .ask(DbExecute {
            sql: sql.to_string(),
            params,
        })
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))
}

fn text(row: &[crate::db::Value], index: usize) -> Result<String, AuthError> {
    row_value(row, index)
        .and_then(ValueExt::as_string)
        .map_err(|error| AuthError::DatabaseError(error.to_string()))
}

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
            // JID localparts and domains compare case-insensitively; native
            // usernames keep the case they were registered with.
            sql: "SELECT 1 FROM users WHERE lower(xmpp_localpart) = lower(?) \
                  UNION ALL \
                  SELECT 1 FROM native_users WHERE lower(username) = lower(?) AND lower(domain) = lower(?) \
                  LIMIT 1"
                .to_string(),
            params: vec![localpart.into(), localpart.into(), domain.into()],
        })
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;

    Ok(row.is_some())
}

/// Returns `true` when `jid` is a registered account on `local_domain`.
/// Waddle has no s2s, so a JID on any other domain (a room, an extension
/// bot) or one without a localpart is never an account.
pub async fn local_account_jid_exists(
    actor: &ActorRef<DbActor>,
    jid: &jid::BareJid,
    local_domain: &str,
) -> Result<bool, AuthError> {
    match jid.node() {
        Some(node) if jid.domain().as_str() == local_domain => {
            local_account_exists(actor, node.as_str(), local_domain).await
        }
        _ => Ok(false),
    }
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
