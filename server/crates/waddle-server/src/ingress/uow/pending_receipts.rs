//! Typed pending-delivery storage operations under the ingress canonical lock.
use super::IngressUowTransaction;
use crate::pending_delivery::database;
use waddle_xmpp::pending_delivery::{
    storage::PendingStorageError, InsertOutcome, PendingRow, PendingRowId, QuotaPolicy,
};

pub(crate) struct PendingReceiptRepository;

pub(crate) struct ArchivedPendingCustody {
    pub row_id: PendingRowId,
    pub notification_outboxed: bool,
}

impl PendingReceiptRepository {
    pub(crate) async fn contains(
        tx: &mut IngressUowTransaction<'_>,
        id: &PendingRowId,
    ) -> Result<bool, PendingStorageError> {
        database::contains_in_transaction(tx.transaction_mut(), id).await
    }

    /// Reuse custody for the same recipient archive copy across resource
    /// handoffs. Hold the pending row through ingress settlement so a consumer
    /// cannot delete it between proving custody and recording route progress.
    pub(crate) async fn archived_custody(
        tx: &mut IngressUowTransaction<'_>,
        recipient: &jid::BareJid,
        stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    ) -> Result<Option<ArchivedPendingCustody>, super::IngressUowError> {
        let postgres = tx.transaction_mut().driver() == crate::db::DatabaseDriver::Postgres;
        if postgres {
            // Same recipient lock as insert_in_transaction, including the
            // absent-row case and competing canonical message transactions.
            tx.transaction_mut()
                .execute(
                    "SELECT pg_advisory_xact_lock(hashtext(?))",
                    crate::db_params![recipient.to_string()],
                )
                .await?;
        }
        let sql = if postgres {
            "SELECT row_id, notification_outboxed_at_ms FROM pending_delivery WHERE recipient_jid = ? AND payload_kind = 'archived' AND archive_stanza_by = ? AND archive_stanza_id = ? FOR UPDATE"
        } else {
            "SELECT row_id, notification_outboxed_at_ms FROM pending_delivery WHERE recipient_jid = ? AND payload_kind = 'archived' AND archive_stanza_by = ? AND archive_stanza_id = ?"
        };
        let mut rows = tx
            .transaction_mut()
            .query(
                sql,
                crate::db_params![
                    recipient.to_string(),
                    stanza_id.by.to_bare().to_string(),
                    stanza_id.id.clone()
                ],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| {
                Ok(ArchivedPendingCustody {
                    row_id: PendingRowId::new(row.get::<String>(0)?),
                    notification_outboxed: row.get::<Option<i64>>(1)?.is_some(),
                })
            })
            .transpose()
    }

    pub(crate) async fn insert(
        tx: &mut IngressUowTransaction<'_>,
        row: &PendingRow,
        quota: QuotaPolicy,
    ) -> Result<InsertOutcome, PendingStorageError> {
        database::insert_in_transaction(tx.transaction_mut(), row, quota, None).await
    }

    pub(crate) async fn mark_notification_outboxed(
        tx: &mut IngressUowTransaction<'_>,
        id: &PendingRowId,
    ) -> Result<u64, PendingStorageError> {
        database::mark_notification_outboxed_in_transaction(tx.transaction_mut(), id).await
    }
}
