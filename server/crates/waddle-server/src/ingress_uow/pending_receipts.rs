//! Typed pending-delivery storage operations under the ingress canonical lock.
use super::IngressUowTransaction;
use crate::pending_delivery::database;
use waddle_xmpp::pending_delivery::{
    storage::PendingStorageError, InsertOutcome, PendingRow, PendingRowId, QuotaPolicy,
};

pub(crate) struct PendingReceiptRepository;

impl PendingReceiptRepository {
    pub(crate) async fn contains(
        tx: &mut IngressUowTransaction<'_>,
        id: &PendingRowId,
    ) -> Result<bool, PendingStorageError> {
        database::contains_in_transaction(tx.transaction_mut(), id).await
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
