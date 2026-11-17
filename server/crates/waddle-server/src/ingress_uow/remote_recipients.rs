//! The local account a remote sender's direct message is addressed to,
//! asserted in the acceptance transaction (ADR-0020 D7, D8; gate G18).

use jid::BareJid;

use super::{IngressUowError, IngressUowTransaction};
use crate::db::DatabaseDriver;
use crate::permissions::host_grant::{ServedAccountDomains, SubjectHome};

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
    /// Assert that `recipient` is an account of this deployment, at one of
    /// the `served` account domains, that has not been retired.
    ///
    /// The account is looked up by the keys every existence check reads
    /// (`auth::directory::account_lookup_keys`): its localpart against the
    /// account rows' lookup key, its bare JID against the native rows'. The
    /// account side's check before the acceptance reads the same keys, so
    /// the two agree on every row; a row without a lookup key, which only a
    /// case-variant twin of a keyed row is after the startup pass, is no
    /// account to either.
    ///
    /// On PostgreSQL the row found is locked `FOR KEY SHARE` until the
    /// acceptance commits: a retirement deletes the row, and that delete
    /// waits for the lock, while an update of the row that changes no key
    /// column does not. A retirement (whose ledger row commits with that delete)
    /// therefore either committed before this read and is seen, or retires
    /// the account after the acceptance committed. SQLite runs one writing
    /// transaction at a time. A native account without an account row is an
    /// account too, as for an extension's requester.
    pub async fn assert_recipient(
        tx: &mut IngressUowTransaction<'_>,
        recipient: &BareJid,
        served: &ServedAccountDomains,
    ) -> Result<RecipientAssertion, IngressUowError> {
        if served.home_of(recipient) != SubjectHome::Served {
            return Ok(RecipientAssertion::Unknown);
        }
        let Some((localpart, jid_key)) =
            crate::auth::directory::account_lookup_keys(recipient, recipient.domain().as_str())
        else {
            return Ok(RecipientAssertion::Unknown);
        };
        let Some(ledger_key) = crate::auth::retirement::retirement_key_of(recipient) else {
            return Ok(RecipientAssertion::Unknown);
        };
        let account = dialect_sql(
            tx,
            "SELECT 1 FROM users WHERE localpart_key = ? FOR KEY SHARE",
            "SELECT 1 FROM users WHERE localpart_key = ?",
        );
        let mut rows = tx
            .transaction_mut()
            .query(account, crate::db_params![localpart])
            .await?;
        let held = rows.next().await?.is_some();
        drop(rows);
        if !held {
            let native = dialect_sql(
                tx,
                "SELECT 1 FROM native_users WHERE jid_key = ? FOR KEY SHARE",
                "SELECT 1 FROM native_users WHERE jid_key = ?",
            );
            let mut rows = tx
                .transaction_mut()
                .query(native, crate::db_params![jid_key])
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
                crate::db_params![ledger_key],
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
