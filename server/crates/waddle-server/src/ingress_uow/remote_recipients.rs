//! The local account a remote sender's direct message is addressed to,
//! asserted in the acceptance transaction (ADR-0020 D7, D8; gate G18).

use jid::BareJid;

use super::{IngressUowError, IngressUowTransaction};
use crate::db::DatabaseDriver;

/// Whether the recipient of a remote sender's direct message is an account
/// of this deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientAssertion {
    /// An account row holds the address and the ledger of retired JIDs does
    /// not.
    Asserted,
    /// No account holds the address, or the ledger holds it.
    Unknown,
}

/// Reads of the account a remote sender writes to.
#[derive(Debug, Default, Clone, Copy)]
pub struct RemoteRecipientRepository;

impl RemoteRecipientRepository {
    /// Assert that `recipient` is an account of this deployment that has not
    /// been retired.
    ///
    /// On PostgreSQL the account row is locked `FOR SHARE` until the
    /// acceptance commits, so a retirement (whose delete of the row waits for
    /// the lock, and whose ledger row commits with that delete) either
    /// committed before this read and is seen, or retires the account after
    /// the acceptance committed. SQLite runs one writing transaction at a
    /// time. A native account without an account row is an account too, as
    /// for an extension's requester.
    pub async fn assert_recipient(
        tx: &mut IngressUowTransaction<'_>,
        recipient: &BareJid,
    ) -> Result<RecipientAssertion, IngressUowError> {
        let Some(key) = crate::auth::retirement::retirement_key_of(recipient) else {
            return Ok(RecipientAssertion::Unknown);
        };
        let account = dialect_sql(
            tx,
            "SELECT jid FROM users WHERE jid = ? FOR SHARE",
            "SELECT jid FROM users WHERE jid = ?",
        );
        let mut rows = tx
            .transaction_mut()
            .query(account, crate::db_params![recipient.to_string()])
            .await?;
        let held = rows.next().await?.is_some();
        drop(rows);
        if !held {
            let native = dialect_sql(
                tx,
                "SELECT 1 FROM native_users WHERE jid_key = ? FOR SHARE",
                "SELECT 1 FROM native_users WHERE jid_key = ?",
            );
            let mut rows = tx
                .transaction_mut()
                .query(native, crate::db_params![key.as_str()])
                .await?;
            let native_held = rows.next().await?.is_some();
            drop(rows);
            if !native_held {
                return Ok(RecipientAssertion::Unknown);
            }
        }
        let mut rows = tx
            .transaction_mut()
            .query(
                "SELECT 1 FROM retired_account_jids WHERE jid_key = ?",
                crate::db_params![key.as_str()],
            )
            .await?;
        let retired = rows.next().await?.is_some();
        drop(rows);
        Ok(if retired {
            RecipientAssertion::Unknown
        } else {
            RecipientAssertion::Asserted
        })
    }
}

fn dialect_sql(
    tx: &mut IngressUowTransaction<'_>,
    postgres: &'static str,
    sqlite: &'static str,
) -> &'static str {
    match tx.transaction_mut().driver() {
        DatabaseDriver::Postgres => postgres,
        DatabaseDriver::Sqlite => sqlite,
    }
}
