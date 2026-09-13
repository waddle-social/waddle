//! Atomic notification candidate and recovery writes beneath the canonical lock.
use waddle_xmpp::{inbox::storage::GroupchatNotificationRecoveryKey, ingress::MessageKey};

use super::{IngressUowError, IngressUowTransaction};
use crate::notification_outbox::{
    NotificationCandidate, NotificationCandidateInsertOutcome, NotificationOutboxStore,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryCompletion {
    Completed,
    AlreadyCompleted,
    Missing,
}

pub(crate) struct RecoveryReceiptRepository;

impl RecoveryReceiptRepository {
    pub(crate) async fn insert_candidate(
        tx: &mut IngressUowTransaction<'_>,
        candidate: &NotificationCandidate,
        created_at_ms: i64,
    ) -> Result<NotificationCandidateInsertOutcome, IngressUowError> {
        NotificationOutboxStore::insert_candidate_in_transaction(
            tx.transaction_mut(),
            candidate,
            created_at_ms,
        )
        .await
        .map_err(Into::into)
    }

    pub(crate) async fn complete(
        tx: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        key: &GroupchatNotificationRecoveryKey,
    ) -> Result<RecoveryCompletion, IngressUowError> {
        let changed = tx.transaction_mut().execute(
            "UPDATE groupchat_notification_recovery SET completed_at_ms = ? WHERE message_key = ? AND recipient_bare_jid = ? AND room_jid = ? AND thread_id = ? AND stanza_id_by = ? AND stanza_id = ? AND completed_at_ms IS NULL",
            crate::db_params![crate::time::now_ms(), message_key.to_storage().to_string(), key.recipient.to_string(), key.room.to_string(), key.thread_id.clone().unwrap_or_default(), key.archive_stanza_id.by.to_string(), key.archive_stanza_id.id.clone()],
        ).await?;
        if changed > 0 {
            return Ok(RecoveryCompletion::Completed);
        }
        Ok(if Self::is_completed(tx, message_key, key).await? {
            RecoveryCompletion::AlreadyCompleted
        } else {
            RecoveryCompletion::Missing
        })
    }

    pub(crate) async fn is_completed(
        tx: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        key: &GroupchatNotificationRecoveryKey,
    ) -> Result<bool, IngressUowError> {
        let mut rows = tx.transaction_mut().query(
            "SELECT 1 FROM groupchat_notification_recovery WHERE message_key = ? AND recipient_bare_jid = ? AND room_jid = ? AND thread_id = ? AND stanza_id_by = ? AND stanza_id = ? AND completed_at_ms IS NOT NULL",
            crate::db_params![message_key.to_storage().to_string(), key.recipient.to_string(), key.room.to_string(), key.thread_id.clone().unwrap_or_default(), key.archive_stanza_id.by.to_string(), key.archive_stanza_id.id.clone()],
        ).await?;
        Ok(rows.next().await?.is_some())
    }

    pub(crate) async fn delete(
        tx: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        key: &GroupchatNotificationRecoveryKey,
    ) -> Result<(), IngressUowError> {
        tx.transaction_mut().execute(
            "DELETE FROM groupchat_notification_recovery WHERE message_key = ? AND recipient_bare_jid = ? AND room_jid = ? AND thread_id = ? AND stanza_id_by = ? AND stanza_id = ?",
            crate::db_params![message_key.to_storage().to_string(), key.recipient.to_string(), key.room.to_string(), key.thread_id.clone().unwrap_or_default(), key.archive_stanza_id.by.to_string(), key.archive_stanza_id.id.clone()],
        ).await?;
        Ok(())
    }
}
