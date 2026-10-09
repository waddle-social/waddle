//! Retention pruning of published/failed jobs and outboxed candidates.

use super::*;

impl NotificationOutboxStore {
    pub async fn prune_completed_before(
        &self,
        cutoff_ms: i64,
        batch_size: usize,
    ) -> Result<NotificationOutboxPruneOutcome, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 10_000);
        let has_foundation = self.has_canonical_descendants().await?;
        let jobs_guard = if has_foundation {
            "AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = notification_outbox.job_id)"
        } else {
            ""
        };
        let candidates_deleted = self
            .prune_outboxed_candidates_before(cutoff_ms, batch_size, has_foundation)
            .await?;
        let jobs_deleted = self
            .execute(
                &format!(r#"
                DELETE FROM notification_outbox
                WHERE job_id IN (
                    SELECT job_id
                    FROM notification_outbox
                    WHERE status IN (?, ?)
                      AND updated_at_ms < ?
                      {jobs_guard}
                      AND NOT EXISTS (SELECT 1 FROM notification_outbox_lineage AS lineage WHERE lineage.job_id = notification_outbox.job_id AND (lineage.settled_at_ms IS NULL OR lineage.settled_at_ms > ?))
                    ORDER BY updated_at_ms ASC, job_id ASC
                    LIMIT ?
                )
                "#),
                crate::db_params![
                    STATUS_PUBLISHED,
                    STATUS_FAILED,
                    cutoff_ms,
                    cutoff_ms.min(crate::time::now_ms().saturating_sub(8 * 24 * 60 * 60 * 1_000)),
                    batch_size as i64,
                ],
            )
            .await?;
        self.execute("DELETE FROM notification_outbox_lineage WHERE settled_at_ms IS NOT NULL AND settled_at_ms < ? AND NOT EXISTS (SELECT 1 FROM notification_candidates WHERE delivery_id = notification_outbox_lineage.candidate_delivery_id) AND NOT EXISTS (SELECT 1 FROM notification_outbox WHERE job_id = notification_outbox_lineage.job_id)",
            crate::db_params![cutoff_ms.min(crate::time::now_ms().saturating_sub(8 * 24 * 60 * 60 * 1_000))]).await?;
        Ok(NotificationOutboxPruneOutcome {
            candidates_deleted,
            jobs_deleted,
        })
    }

    async fn prune_outboxed_candidates_before(
        &self,
        cutoff_ms: i64,
        batch_size: usize,
        has_foundation: bool,
    ) -> Result<u64, NotificationOutboxError> {
        let candidates_guard = if has_foundation {
            "AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = notification_candidates.delivery_id)"
        } else {
            ""
        };
        self.execute(
            &format!(r#"
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
                      {candidates_guard}
                      AND NOT EXISTS (SELECT 1 FROM notification_outbox_lineage AS lineage WHERE lineage.candidate_delivery_id = notification_candidates.delivery_id AND (lineage.settled_at_ms IS NULL OR lineage.settled_at_ms > ?))
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
                "#),
            crate::db_params![cutoff_ms, cutoff_ms.min(crate::time::now_ms().saturating_sub(8 * 24 * 60 * 60 * 1_000)), batch_size as i64],
        )
        .await
    }
    async fn has_canonical_descendants(&self) -> Result<bool, NotificationOutboxError> {
        if self.db.driver() == crate::db::DatabaseDriver::Postgres {
            let mut rows = self
                .query("SELECT to_regclass('ingress_effect_descendants')::text", ())
                .await?;
            Ok(match rows.next().await? {
                Some(row) => row.get::<Option<String>>(0)?.is_some(),
                None => false,
            })
        } else {
            let mut rows = self.query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ingress_effect_descendants'", ()).await?;
            Ok(rows.next().await?.is_some())
        }
    }
}
