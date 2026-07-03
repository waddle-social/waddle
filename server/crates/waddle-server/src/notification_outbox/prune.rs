//! Retention pruning of published/failed jobs and outboxed candidates.

use super::*;

impl NotificationOutboxStore {
    pub async fn prune_completed_before(
        &self,
        cutoff_ms: i64,
        batch_size: usize,
    ) -> Result<NotificationOutboxPruneOutcome, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 10_000);
        let candidates_deleted = self
            .prune_outboxed_candidates_before(cutoff_ms, batch_size)
            .await?;
        let jobs_deleted = self
            .execute(
                r#"
                DELETE FROM notification_outbox
                WHERE job_id IN (
                    SELECT job_id
                    FROM notification_outbox
                    WHERE status IN (?, ?)
                      AND updated_at_ms < ?
                    ORDER BY updated_at_ms ASC, job_id ASC
                    LIMIT ?
                )
                "#,
                crate::db_params![
                    STATUS_PUBLISHED,
                    STATUS_FAILED,
                    cutoff_ms,
                    batch_size as i64,
                ],
            )
            .await?;
        Ok(NotificationOutboxPruneOutcome {
            candidates_deleted,
            jobs_deleted,
        })
    }

    async fn prune_outboxed_candidates_before(
        &self,
        cutoff_ms: i64,
        batch_size: usize,
    ) -> Result<u64, NotificationOutboxError> {
        self.execute(
            r#"
                DELETE FROM notification_candidates
                WHERE (
                    recipient_bare_jid,
                    conversation_jid,
                    sender_jid,
                    thread_id,
                    stanza_id_by,
                    stanza_id,
                    class
                ) IN (
                    SELECT recipient_bare_jid,
                           conversation_jid,
                           sender_jid,
                           thread_id,
                           stanza_id_by,
                           stanza_id,
                           class
                    FROM notification_candidates
                    WHERE outboxed_at_ms IS NOT NULL
                      AND outboxed_at_ms < ?
                    ORDER BY outboxed_at_ms ASC,
                             recipient_bare_jid ASC,
                             conversation_jid ASC,
                             sender_jid ASC,
                             thread_id ASC,
                             stanza_id_by ASC,
                             stanza_id ASC,
                             class ASC
                    LIMIT ?
                )
                "#,
            crate::db_params![cutoff_ms, batch_size as i64],
        )
        .await
    }
}
