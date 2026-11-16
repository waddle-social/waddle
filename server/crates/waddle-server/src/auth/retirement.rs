//! Durable non-reuse of retired account JIDs (ADR-0020 D8).
//!
//! Grants follow JID semantics: whoever holds a JID holds the grants issued
//! to it, on this deployment and on every other one. A home therefore never
//! issues the JID of a retired account to a new holder.
//!
//! [`retire_account`] records the JID in `retired_account_jids` in the
//! transaction that removes the account's rows. Every path that provisions
//! an account or binds a JID to one reads that ledger where it would create
//! the row, and creates nothing for a JID it holds:
//!
//! - native registration ([`super::NativeUserStore::register`]) inserts the
//!   account only where the ledger does not hold its JID
//!   ([`JID_NOT_RETIRED_SQL`]), and answers
//!   [`super::AuthError::JidRetired`];
//! - provisioning at an identity-provider login
//!   (`IdentityService::create_user`) inserts under the same condition and
//!   treats a retired name as a taken one: the account gets the next free
//!   name, never the retired JID, also when the same provider subject
//!   returns;
//! - session creation (`db::actor::CreateAuthSession`), which inserts an
//!   account row for a JID that has none, reads the ledger in its
//!   transaction and creates neither the row nor the session.
//!
//! A row of the ledger is never updated or deleted. Retirement is an
//! operator's act (`waddle-server account retire`): on a restricted
//! PostgreSQL runtime the runtime role may read the ledger and nothing else,
//! and may not delete an account row.
//!
//! Retirement and provisioning exclude each other. On PostgreSQL the
//! retiring transaction takes `SHARE ROW EXCLUSIVE` on both account tables
//! before it writes: every insert that has not committed finishes first and
//! is then removed with the account, and every insert that arrives later
//! waits for the commit and runs against the ledger that holds the JID.
//! SQLite runs one writing transaction at a time.

use chrono::Utc;
use jid::BareJid;

use super::directory::canonical_account_jid;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

/// The condition a provisioning statement carries: the ledger does not hold
/// the JID. Its one parameter is the JID's canonical form
/// ([`retirement_key`]).
pub(crate) const JID_NOT_RETIRED_SQL: &str =
    "NOT EXISTS (SELECT 1 FROM retired_account_jids WHERE jid_key = ?)";

/// What a session creation is refused with for a retired JID, as the text of
/// a [`DatabaseError::QueryFailed`]. It names no JID.
pub(crate) const RETIRED_JID_REFUSAL: &str = "the account JID is retired";

/// The ledger's key for the account `localpart@domain`: its bare JID in the
/// identity profile's canonical form. `None` when the name has none, and
/// then no account can hold it either.
pub(crate) fn retirement_key(localpart: &str, domain: &str) -> Option<String> {
    canonical_account_jid(localpart, domain).map(|jid| jid.to_string())
}

/// The ledger's key for a bare account JID.
pub(crate) fn retirement_key_of(jid: &BareJid) -> Option<String> {
    retirement_key(jid.node()?.as_str(), jid.domain().as_str())
}

/// Why an account could not be retired.
#[derive(Debug, thiserror::Error)]
pub enum RetireAccountError {
    #[error("the JID names no account: it needs a localpart and a canonical form")]
    NotAnAccountJid,
    #[error("a statement of the retirement failed")]
    Database(#[from] DatabaseError),
}

/// What a retirement did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRetirement {
    /// The retired JID, in canonical form.
    pub jid: String,
    /// Whether this call recorded the retirement; `false` when the ledger
    /// already held the JID.
    pub newly_retired: bool,
    /// Native account rows removed.
    pub native_accounts: u64,
    /// Account rows removed, with their identity-provider links and
    /// sessions.
    pub accounts: u64,
}

/// Retire the account `jid`: record its JID in the ledger and remove its
/// native credentials, its account row, its identity-provider links and its
/// sessions, in one transaction. A JID that holds no account is recorded as
/// well, so that it is never issued. Retiring a retired JID removes whatever
/// rows it still finds and changes nothing else.
///
/// The caller's database role must be able to delete from both account
/// tables: the schema owner, or a runtime that owns its schema. It does not
/// end the account's live streams and does not touch the account's grants
/// in any room; see the retirement runbook.
pub async fn retire_account(
    db: &Database,
    jid: &BareJid,
) -> Result<AccountRetirement, RetireAccountError> {
    let node = jid.node().ok_or(RetireAccountError::NotAnAccountJid)?;
    let canonical = canonical_account_jid(node.as_str(), jid.domain().as_str())
        .ok_or(RetireAccountError::NotAnAccountJid)?;
    let localpart_key = canonical
        .node()
        .ok_or(RetireAccountError::NotAnAccountJid)?
        .as_str()
        .to_owned();
    let key = canonical.to_string();

    let mut tx = match db.driver() {
        DatabaseDriver::Postgres => {
            let mut tx = db.begin().await?;
            // Provisioning inserts into these tables; the lock makes each
            // one finish before this transaction writes, or wait for it.
            tx.execute(
                "LOCK TABLE users, native_users IN SHARE ROW EXCLUSIVE MODE",
                (),
            )
            .await?;
            tx
        }
        DatabaseDriver::Sqlite => db.begin_immediate().await?,
    };
    let outcome = retire_in(&mut tx, &canonical, &key, &localpart_key).await;
    match outcome {
        Ok(retirement) => {
            tx.commit().await?;
            Ok(retirement)
        }
        Err(error) => {
            // The rollback's own failure changes nothing that was written.
            let _ = tx.rollback().await;
            Err(error.into())
        }
    }
}

async fn retire_in(
    tx: &mut Transaction<'_>,
    canonical: &BareJid,
    key: &str,
    localpart_key: &str,
) -> Result<AccountRetirement, DatabaseError> {
    let recorded = tx
        .execute(
            "INSERT INTO retired_account_jids (jid_key, localpart_key, retired_at_ms) \
             VALUES (?, ?, ?) ON CONFLICT (jid_key) DO NOTHING",
            crate::db_params![key, localpart_key, Utc::now().timestamp_millis()],
        )
        .await?;

    // Native accounts: by the lookup key, and the rows written without one.
    let mut native_accounts = tx
        .execute(
            "DELETE FROM native_users WHERE jid_key = ?",
            crate::db_params![key],
        )
        .await?;
    let mut unkeyed = Vec::new();
    let mut rows = tx
        .query(
            "SELECT id, username, domain FROM native_users WHERE jid_key IS NULL",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let id: i64 = row.get(0)?;
        let username: String = row.get(1)?;
        let domain: String = row.get(2)?;
        if canonical_account_jid(&username, &domain).as_ref() == Some(canonical) {
            unkeyed.push(id);
        }
    }
    drop(rows);
    for id in unkeyed {
        native_accounts += tx
            .execute(
                "DELETE FROM native_users WHERE id = ?",
                crate::db_params![id],
            )
            .await?;
    }

    // Account rows carry no domain column: a row is the account's when its
    // own JID has the retired JID's canonical form. Its identity-provider
    // links and its sessions go with it (`ON DELETE CASCADE`).
    let mut held = Vec::new();
    let mut rows = tx
        .query(
            "SELECT jid FROM users WHERE localpart_key = ? OR localpart_key IS NULL",
            crate::db_params![localpart_key],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let stored: String = row.get(0)?;
        let same_account = stored
            .parse::<BareJid>()
            .ok()
            .and_then(|stored| retirement_key_of(&stored))
            .is_some_and(|stored| stored == key);
        if same_account {
            held.push(stored);
        }
    }
    drop(rows);
    let mut accounts = 0;
    for stored in held {
        accounts += tx
            .execute("DELETE FROM users WHERE jid = ?", crate::db_params![stored])
            .await?;
    }

    Ok(AccountRetirement {
        jid: key.to_owned(),
        newly_retired: recorded > 0,
        native_accounts,
        accounts,
    })
}

/// Whether the ledger holds `jid`. A JID without a canonical form names no
/// account and is not retired.
pub async fn jid_is_retired(db: &Database, jid: &BareJid) -> Result<bool, DatabaseError> {
    let Some(key) = retirement_key_of(jid) else {
        return Ok(false);
    };
    let mut tx = db.begin().await?;
    let mut rows = tx
        .query(
            "SELECT 1 FROM retired_account_jids WHERE jid_key = ?",
            crate::db_params![key],
        )
        .await?;
    let retired = rows.next().await?.is_some();
    drop(rows);
    tx.rollback().await?;
    Ok(retired)
}
