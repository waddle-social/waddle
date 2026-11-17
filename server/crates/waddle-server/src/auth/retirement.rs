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
//!
//! Every writer of the account tables (provisioning, and also session
//! creation at every login and changes of native credentials) that arrives
//! while the retirement waits for the lock or holds it is held up until the
//! retirement ends. The retirement waits at most 2 s for the lock and then
//! fails without having changed anything; it can be run again. Holding the
//! lock it runs a few statements, each bounded at 5 s; the transaction has
//! no deadline of its own beyond those.
//!
//! The account's open streams and detached stream-management sessions are
//! held by the replicas that serve them, not by these tables. The operator
//! command ends them in a second step once the retirement committed
//! ([`super::session_endings`]); [`retire_account`] itself leaves them.

use chrono::Utc;
use jid::BareJid;

use std::time::Duration;

use super::directory::canonical_account_jid;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};
use crate::ingress_substrate::set_local_transaction_timeouts;

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

/// Longest a retirement waits for the lock on the account tables. Writers
/// of those tables that arrive meanwhile wait behind it, so this is also the
/// longest a retirement can hold up a login before it gives up.
const RETIREMENT_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest any one statement of a retirement runs.
const RETIREMENT_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Why an account could not be retired.
#[derive(Debug, thiserror::Error)]
pub enum RetireAccountError {
    #[error("the JID names no account: it needs a localpart and a canonical form")]
    NotAnAccountJid,
    #[error("the retirement's lock and statement bounds could not be installed")]
    BoundsUnproven,
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
    /// Messages removed from the account's offline store.
    pub offline_messages: u64,
}

/// Retire the account `jid`: record its JID in the ledger and remove its
/// native credentials, its account row, its identity-provider links, its
/// sessions, its offline store, and what remote senders left for it
/// (recorded subscription requests, remote PubSub subscription records and
/// offline admissions), in one transaction. A JID that holds no account is recorded as
/// well, so that it is never issued. Retiring a retired JID removes whatever
/// rows it still finds and changes nothing else.
///
/// The caller's database role must be able to delete from both account
/// tables: the schema owner, or a runtime that owns its schema. It does not
/// end the account's live streams, which the operator command asks every
/// replica to do after this commits ([`super::session_endings`]), and does
/// not touch the account's grants in any room; see the retirement runbook.
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
            // While this transaction waits for the table lock, PostgreSQL
            // queues every later writer of the account tables behind it,
            // logins included. The wait is bounded, and so is every
            // statement that follows.
            if !set_local_transaction_timeouts(
                &mut tx,
                RETIREMENT_LOCK_TIMEOUT,
                RETIREMENT_STATEMENT_TIMEOUT,
            )
            .await?
            {
                return Err(RetireAccountError::BoundsUnproven);
            }
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

    // The account's offline store: no session of the account will ever
    // flush it, and a row left would wait for the JID's next holder, which
    // the ledger never lets exist. From this commit on the store refuses
    // every row for the account (`crate::pending_delivery`'s retired
    // recipients), whichever producer offers it. An insert serialises with
    // this delete on the advisory lock keyed by the account's canonical
    // JID, which it takes whatever spelling of the JID its row names: one
    // that holds it commits first and its row is removed here, and one that
    // waits for it reads the ledger this transaction wrote.
    if tx.driver() == DatabaseDriver::Postgres {
        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtext(?))",
            crate::db_params![canonical.as_str()],
        )
        .await?;
    }
    let offline_messages = tx
        .execute(
            "DELETE FROM pending_delivery WHERE recipient_jid = ?",
            crate::db_params![canonical.as_str()],
        )
        .await?
        + delete_offline_spellings(tx, canonical, key).await?;

    // What remote senders left for the account (gate G18): their recorded
    // subscription requests, the account's records of remote PubSub
    // subscriptions, and the offline admissions of strangers' messages,
    // keyed by the account's canonical JID.
    for statement in [
        "DELETE FROM s2s_pending_subscriptions WHERE account_jid = ?",
        "DELETE FROM s2s_remote_pubsub_subscriptions WHERE account_jid = ?",
        "DELETE FROM s2s_remote_offline_admissions WHERE account_jid = ?",
    ] {
        tx.execute(statement, crate::db_params![canonical.as_str()])
            .await?;
    }

    Ok(AccountRetirement {
        jid: key.to_owned(),
        newly_retired: recorded > 0,
        native_accounts,
        accounts,
        offline_messages,
    })
}

/// Remove the rows of the account's offline store kept under a spelling of
/// its JID other than `canonical`: every row whose recipient has the
/// ledger key `key`, the key under which the store refuses an insert for
/// the account. A row names its recipient as the `jid` parser prepared it,
/// which keeps a domain's U-labels, and a row written before recipients
/// were prepared may differ in letter case as well. Every such spelling has
/// the canonical localpart, so the candidates are the recipients that start
/// with it, compared without letter case, and the key decides. The read
/// scans the store's recipients under the retirement's statement bound.
async fn delete_offline_spellings(
    tx: &mut Transaction<'_>,
    canonical: &BareJid,
    key: &str,
) -> Result<u64, DatabaseError> {
    let Some(localpart) = canonical.node() else {
        return Ok(0);
    };
    let mut pattern = String::new();
    for character in localpart.as_str().chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push_str("@%");
    let mut spellings = Vec::new();
    let mut rows = tx
        .query(
            "SELECT DISTINCT recipient_jid FROM pending_delivery \
             WHERE lower(recipient_jid) LIKE ? ESCAPE '\\'",
            crate::db_params![pattern],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let stored: String = row.get(0)?;
        let same_account = stored != canonical.as_str()
            && stored
                .parse::<BareJid>()
                .ok()
                .and_then(|stored| retirement_key_of(&stored))
                .is_some_and(|stored| stored == key);
        if same_account {
            spellings.push(stored);
        }
    }
    drop(rows);
    let mut removed = 0;
    for stored in spellings {
        removed += tx
            .execute(
                "DELETE FROM pending_delivery WHERE recipient_jid = ?",
                crate::db_params![stored],
            )
            .await?;
    }
    Ok(removed)
}

/// Why a server refuses to start: its database role cannot read the
/// ledger of retired account JIDs.
#[derive(Debug, thiserror::Error)]
#[error(
    "the runtime database role cannot read retired_account_jids, the ledger of retired account \
     JIDs; the owner phase grants it SELECT, and every account provisioning, session creation \
     and offline-store write reads it: {0}"
)]
pub struct RetiredLedgerUnreadable(#[source] DatabaseError);

/// Read the ledger once, as the role the server runs under, so that a role
/// without `SELECT` on it stops startup with an error that names the
/// ledger, rather than failing every account provisioning, session creation
/// and offline-store write that reads it later.
pub async fn verify_ledger_readable(db: &Database) -> Result<(), RetiredLedgerUnreadable> {
    let read = async {
        let guard = db.guard().await?;
        let mut rows = guard
            .query("SELECT 1 FROM retired_account_jids LIMIT 1", ())
            .await?;
        rows.next().await?;
        Ok::<_, DatabaseError>(())
    };
    read.await.map_err(RetiredLedgerUnreadable)
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
