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
/// without one its canonical lookup key, then drop roster items whose
/// contact is not an account on the owner's domain (rooms, extension bots,
/// names nobody registered) by the same rule the roster set and subscription
/// guards apply. Idempotent.
pub(crate) async fn reconcile_local_accounts(actor: &ActorRef<DbActor>) -> Result<(), AuthError> {
    backfill_account_keys(actor).await?;
    prune_roster_contacts(actor).await
}

/// Each pruned owner also loses its XEP-0237 version, so a cached roster no
/// longer matches and is refetched.
// ponytail: reads every roster row each startup; page by owner if rosters grow large.
async fn prune_roster_contacts(actor: &ActorRef<DbActor>) -> Result<(), AuthError> {
    let items = query(
        actor,
        "SELECT user_jid, contact_jid FROM roster_items",
        vec![],
    )
    .await?;
    let mut accounts = std::collections::HashMap::new();
    for row in items {
        let owner = text(&row, 0)?;
        let contact = text(&row, 1)?;
        let keep = match (
            owner.parse::<jid::BareJid>(),
            contact.parse::<jid::BareJid>(),
        ) {
            (Ok(owner), Ok(contact)) if owner.domain() == contact.domain() => {
                match accounts.get(&contact) {
                    Some(exists) => *exists,
                    None => {
                        let exists =
                            local_account_jid_exists(actor, &contact, contact.domain().as_str())
                                .await?;
                        accounts.insert(contact, exists);
                        exists
                    }
                }
            }
            _ => false,
        };
        if keep {
            continue;
        }
        execute(
            actor,
            "DELETE FROM roster_items WHERE user_jid = ? AND contact_jid = ?",
            vec![owner.as_str().into(), contact.into()],
        )
        .await?;
        execute(
            actor,
            "DELETE FROM roster_versions WHERE user_jid = ?",
            vec![owner.into()],
        )
        .await?;
    }
    Ok(())
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
/// Both sides compare the JID library's canonical keys exactly, so a name
/// matches whatever case or Unicode form it was registered with. `users` rows
/// carry no `domain` column — OIDC accounts are always local to the server's
/// own domain — so they are matched on the localpart alone; native accounts
/// on their bare JID. Callers are expected to have already constrained
/// `domain` to the local server domain (group-DM validation, for example,
/// rejects non-local members before reaching here).
pub async fn local_account_exists(
    actor: &ActorRef<DbActor>,
    localpart: &str,
    domain: &str,
) -> Result<bool, AuthError> {
    let (Some(localpart), Some(jid)) = (
        canonical_localpart(localpart),
        canonical_account_jid(localpart, domain),
    ) else {
        return Ok(false);
    };
    let row = actor
        .ask(DbQueryOne {
            sql: "SELECT 1 FROM users WHERE localpart_key = ? \
                  UNION ALL \
                  SELECT 1 FROM native_users WHERE jid_key = ? \
                  LIMIT 1"
                .to_string(),
            params: vec![localpart.as_str().into(), jid.as_str().into()],
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
