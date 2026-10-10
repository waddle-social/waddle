//! Isolate malformed scheduling data without inventing a terminal disposition.
use super::*;
use crate::db::Row;

pub(super) fn is_candidate_data_error(error: &NotificationOutboxError) -> bool {
    matches!(
        error,
        NotificationOutboxError::InvalidDeliveryIdentity
            | NotificationOutboxError::InvalidClass(_)
            | NotificationOutboxError::InvalidReason(_)
            | NotificationOutboxError::InvalidRecipientBareJid(_)
            | NotificationOutboxError::InvalidConversationJid(_)
            | NotificationOutboxError::InvalidSenderJid(_)
            | NotificationOutboxError::SenderJidMissingResource(_)
            | NotificationOutboxError::SenderConversationMismatch { .. }
            | NotificationOutboxError::InvalidArchiveStanzaIdBy(_)
    )
}

/// Only a vanished scanned intent can accompany a legally collected parent.
pub(super) async fn lock_scanned_parent(
    tx: &mut Transaction<'_>,
    recorded: &Recorded,
) -> Result<bool, IngressUowError> {
    match EffectDescendantRepository::lock_raw(tx, recorded.message).await {
        Ok(()) => Ok(true),
        Err(IngressUowError::EffectIntentMessageMissing) => {
            let query = if tx.driver() == DatabaseDriver::Postgres {
                "SELECT 1 FROM ingress_effect_intents WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ?"
            } else {
                "SELECT 1 FROM ingress_effect_intents WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ?"
            };
            let mut rows = tx
                .query(
                    query,
                    crate::db_params![
                        recorded.message.to_storage().to_string(),
                        recorded.kind,
                        recorded.hash.clone()
                    ],
                )
                .await?;
            if rows.next().await?.is_some() {
                Err(IngressUowError::EffectIntentMessageMissing)
            } else {
                Ok(false)
            }
        }
        Err(error) => Err(error),
    }
}

impl NotificationOutboxStore {
    pub(super) async fn quarantine_recorded_candidate(
        &self,
        recorded: &Recorded,
        candidate_row: &Row,
    ) -> Result<(), IngressUowError> {
        let Some((owner, conversation, archive)) = target(&recorded.intent) else {
            return Ok(());
        };
        // These are untrusted storage key components, used only at this SQL boundary.
        let thread: String = candidate_row.get(3)?;
        let class: String = candidate_row.get(6)?;
        let mut tx = begin_bounded(&self.db).await?;
        if !lock_scanned_parent(&mut tx, recorded).await? {
            tx.commit().await?;
            return Ok(());
        }
        let query = if tx.driver() == DatabaseDriver::Postgres {
            "SELECT suppressed_reason,quarantined_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id_by = ? AND stanza_id = ? AND class = ? FOR UPDATE NOWAIT"
        } else {
            "SELECT suppressed_reason,quarantined_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id_by = ? AND stanza_id = ? AND class = ?"
        };
        let mut rows = tx
            .query(
                query,
                crate::db_params![
                    owner.to_string(),
                    conversation.to_string(),
                    &thread,
                    archive.by.to_string(),
                    &archive.id,
                    &class
                ],
            )
            .await
            .map_err(crate::ingress_uow::canonical_nowait_error)?;
        let Some(row) = rows
            .next()
            .await
            .map_err(crate::ingress_uow::canonical_nowait_error)?
        else {
            tx.commit().await?;
            return Ok(());
        };
        let audit: Option<String> = row.get(0)?;
        let already_quarantined = row.get::<Option<i64>>(1)?.is_some();
        let invalid_audit = audit
            .as_deref()
            .is_some_and(|reason| SuppressedReason::from_db_value(reason).is_err());
        drop(rows);
        let mut hash = Sha256::new();
        hash.update(b"waddle.notification-candidate-quarantine.v1\0");
        hash.update(recorded.message.to_storage().as_bytes());
        hash.update(recorded.kind.to_be_bytes());
        hash.update(&recorded.hash);
        let digest = hash.finalize();
        let mut identity = [0; 16];
        identity.copy_from_slice(&digest[..16]);
        EffectDescendantRepository::attach_raw(
            &mut tx,
            recorded.message,
            &recorded.intent.semantic_key(),
            Uuid::from_bytes(identity),
        )
        .await?;
        // Preserve the unknown audit before removing it from the active CHECK.
        // Neither NULL nor outboxed_at is a receipt for this quarantined history.
        tx.execute("UPDATE notification_candidates SET quarantined_at_ms = COALESCE(quarantined_at_ms, ?), outboxed_at_ms = COALESCE(outboxed_at_ms, ?), quarantined_suppressed_reason = COALESCE(quarantined_suppressed_reason, ?), suppressed_reason = CASE WHEN ? = 1 THEN NULL ELSE suppressed_reason END WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id_by = ? AND stanza_id = ? AND class = ?",
            crate::db_params![crate::time::now_ms(), crate::time::now_ms(), if invalid_audit { audit } else { None }, i64::from(invalid_audit), owner.to_string(), conversation.to_string(), thread, archive.by.to_string(), archive.id.clone(), class]).await?;
        tx.commit().await?;
        if !already_quarantined {
            tracing::warn!("quarantined malformed legacy notification candidate; canonical custody remains pending");
        }
        Ok(())
    }
}
