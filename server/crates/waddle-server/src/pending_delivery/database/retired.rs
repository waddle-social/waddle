//! The offline store of a retired account stays empty (ADR-0020 D8).
//!
//! The retirement of an account records its JID in `retired_account_jids`
//! and removes its offline store in one transaction
//! (`crate::auth::retirement`). From then on every insert for the account
//! is refused here, whichever producer makes it: an SM promotion, the
//! recovery of ingress custody, the teardown of a live stream, an ingress
//! acceptance or a remote sender's message. Nothing a retired account's
//! sessions held or are sent is kept for a session it will never have.
//!
//! On PostgreSQL an insert and the retirement serialise on a
//! transaction-scoped advisory lock keyed by the account's canonical JID,
//! the form the retirement removes rows by: an insert that took it first
//! commits before the retirement's delete reads the store, and one that
//! waits for it reads the ledger the retirement committed. SQLite runs one
//! writing transaction at a time.
//!
//! The ledger lives in the global database, which the offline store shares
//! in every deployment (startup refuses a split store). A store opened on a
//! database without the ledger (a disposable test store) refuses no
//! recipient.

use jid::BareJid;

use super::{DatabaseDriver, PendingStorageError, Transaction};
use crate::auth::retirement::retirement_key_of;

fn storage_error(error: crate::db::DatabaseError) -> PendingStorageError {
    PendingStorageError::Other(error.to_string())
}

/// The lock key that serialises an insert for `recipient` with the
/// retirement of its account, when it differs from `recipient` as written:
/// the account's canonical JID.
pub(super) fn retirement_lock_key(recipient: &BareJid) -> Option<String> {
    retirement_key_of(recipient).filter(|key| *key != recipient.to_string())
}

/// Whether the transaction's database holds the ledger of retired account
/// JIDs.
async fn ledger_present(tx: &mut Transaction<'_>) -> Result<bool, PendingStorageError> {
    let sql = match tx.driver() {
        DatabaseDriver::Postgres => {
            "SELECT 1 WHERE to_regclass('retired_account_jids') IS NOT NULL"
        }
        DatabaseDriver::Sqlite => {
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'retired_account_jids'"
        }
    };
    let mut rows = tx.query(sql, ()).await.map_err(storage_error)?;
    Ok(rows.next().await.map_err(storage_error)?.is_some())
}

/// Whether the ledger holds the account `recipient` names. The caller holds
/// the recipient's advisory lock on PostgreSQL, so that a retirement either
/// committed before this read or waits for the caller's commit.
pub(super) async fn recipient_retired_in(
    tx: &mut Transaction<'_>,
    recipient: &BareJid,
) -> Result<bool, PendingStorageError> {
    let Some(key) = retirement_key_of(recipient) else {
        return Ok(false);
    };
    if !ledger_present(tx).await? {
        return Ok(false);
    }
    let mut rows = tx
        .query(
            "SELECT 1 FROM retired_account_jids WHERE jid_key = ?",
            crate::db_params![key],
        )
        .await
        .map_err(storage_error)?;
    Ok(rows.next().await.map_err(storage_error)?.is_some())
}
