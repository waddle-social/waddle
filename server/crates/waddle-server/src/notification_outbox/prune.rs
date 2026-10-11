//! Retention pruning of published/failed jobs and outboxed candidates.

use super::*;

impl NotificationOutboxStore {
    pub async fn prune_completed_before(
        &self,
        cutoff_ms: i64,
        batch_size: usize,
    ) -> Result<NotificationOutboxPruneOutcome, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 10_000);
        let tail_cutoff = cutoff_ms.min(
            crate::time::now_ms()
                .saturating_sub(crate::ingress_substrate::ALIAS_RETENTION.num_milliseconds()),
        );
        let has_foundation = self.has_canonical_descendants().await?;
        let candidates_deleted = self
            .prune_outboxed_candidates_before(cutoff_ms, tail_cutoff, batch_size, has_foundation)
            .await?;
        let jobs_deleted = self
            .prune_jobs_before(cutoff_ms, tail_cutoff, batch_size, has_foundation)
            .await?;
        let mut tx = self.begin_prune_transaction().await?;
        tx.execute("DELETE FROM notification_outbox_lineage WHERE settled_at_ms IS NOT NULL AND settled_at_ms < ? AND NOT EXISTS (SELECT 1 FROM notification_candidates WHERE delivery_id = notification_outbox_lineage.candidate_delivery_id) AND NOT EXISTS (SELECT 1 FROM notification_outbox WHERE job_id = notification_outbox_lineage.job_id)",
            crate::db_params![tail_cutoff]).await?;
        tx.commit().await?;
        Ok(NotificationOutboxPruneOutcome {
            candidates_deleted,
            jobs_deleted,
        })
    }

    async fn prune_outboxed_candidates_before(
        &self,
        cutoff_ms: i64,
        tail_cutoff: i64,
        batch_size: usize,
        has_foundation: bool,
    ) -> Result<u64, NotificationOutboxError> {
        let foundation_guard = if has_foundation {
            "AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = notification_candidates.delivery_id)"
        } else {
            ""
        };
        let guards = format!(
            "outboxed_at_ms IS NOT NULL AND quarantined_at_ms IS NULL AND outboxed_at_ms < ? {foundation_guard} AND NOT EXISTS (SELECT 1 FROM notification_outbox_lineage AS lineage WHERE lineage.candidate_delivery_id = notification_candidates.delivery_id AND (lineage.settled_at_ms IS NULL OR lineage.settled_at_ms > ?))"
        );
        let mut tx = self.begin_prune_transaction().await?;
        let mut rows = tx.query(
            &format!("SELECT recipient_bare_jid, conversation_jid, sender_jid, thread_id, stanza_id_by, stanza_id, class, delivery_id FROM notification_candidates WHERE {guards} ORDER BY outboxed_at_ms, recipient_bare_jid, conversation_jid, sender_jid, thread_id, stanza_id_by, stanza_id, class LIMIT ?"),
            crate::db_params![cutoff_ms, tail_cutoff, batch_size as i64],
        ).await?;
        let mut selected = Vec::new();
        while let Some(row) = rows.next().await? {
            selected.push(row);
        }
        drop(rows);
        tx.commit().await?;
        let mut deleted = 0;
        for row in selected {
            match self
                .prune_candidate_row(&row, &guards, cutoff_ms, tail_cutoff, has_foundation)
                .await
            {
                Ok(count) => deleted += count,
                Err(error) if prune_canonical_contention(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(deleted)
    }

    async fn prune_candidate_row(
        &self,
        row: &Row,
        guards: &str,
        cutoff_ms: i64,
        tail_cutoff: i64,
        has_foundation: bool,
    ) -> Result<u64, NotificationOutboxError> {
        // Legacy NULL delivery identities remain addressable by their complete
        // stored dedup tuple. Text stays at this database binding boundary.
        let recipient: String = row.get(0)?;
        let conversation: String = row.get(1)?;
        let sender: String = row.get(2)?;
        let thread: String = row.get(3)?;
        let stanza_by: String = row.get(4)?;
        let stanza: String = row.get(5)?;
        let class: String = row.get(6)?;
        let delivery: Option<String> = row.get(7)?;
        let identity = "recipient_bare_jid = ? AND conversation_jid = ? AND sender_jid = ? AND thread_id = ? AND stanza_id_by = ? AND stanza_id = ? AND class = ?";
        let mut tx = self.begin_prune_transaction().await?;
        prelock_prune_ancestry(&mut tx, delivery.as_deref(), has_foundation).await?;
        let lock = prune_row_lock(tx.driver());
        let mut rows = tx
            .query(
                &format!("SELECT delivery_id FROM notification_candidates WHERE {identity}{lock}"),
                crate::db_params![
                    &recipient,
                    &conversation,
                    &sender,
                    &thread,
                    &stanza_by,
                    &stanza,
                    &class
                ],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(0);
        };
        let current_delivery: Option<String> = row.get(0)?;
        drop(rows);
        // Recheck after the child lock. Any newly discovered canonical parent
        // is acquired NOWAIT, so this lock cannot introduce an inverted wait.
        prelock_prune_ancestry(&mut tx, current_delivery.as_deref(), has_foundation).await?;
        let deleted = tx
            .execute(
                &format!("DELETE FROM notification_candidates WHERE {identity} AND {guards}"),
                crate::db_params![
                    &recipient,
                    &conversation,
                    &sender,
                    &thread,
                    &stanza_by,
                    &stanza,
                    &class,
                    cutoff_ms,
                    tail_cutoff
                ],
            )
            .await?;
        tx.commit().await?;
        Ok(deleted)
    }

    async fn prune_jobs_before(
        &self,
        cutoff_ms: i64,
        tail_cutoff: i64,
        batch_size: usize,
        has_foundation: bool,
    ) -> Result<u64, NotificationOutboxError> {
        let foundation_guard = if has_foundation {
            "AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = notification_outbox.job_id)"
        } else {
            ""
        };
        let guards = format!(
            "status IN (?, ?) AND (status != '{STATUS_FAILED}' OR queue_acceptance_may_exist = 0) AND updated_at_ms < ? {foundation_guard} AND NOT EXISTS (SELECT 1 FROM notification_outbox_lineage AS lineage WHERE lineage.job_id = notification_outbox.job_id AND (lineage.settled_at_ms IS NULL OR lineage.settled_at_ms > ?))"
        );
        let mut tx = self.begin_prune_transaction().await?;
        let mut rows = tx.query(
            &format!("SELECT job_id FROM notification_outbox WHERE {guards} ORDER BY updated_at_ms, job_id LIMIT ?"),
            crate::db_params![STATUS_PUBLISHED, STATUS_FAILED, cutoff_ms, tail_cutoff, batch_size as i64],
        ).await?;
        let mut selected = Vec::new();
        while let Some(row) = rows.next().await? {
            selected.push(row.get::<String>(0)?);
        }
        drop(rows);
        tx.commit().await?;
        let mut deleted = 0;
        for job in selected {
            match self
                .prune_job_row(&job, &guards, cutoff_ms, tail_cutoff, has_foundation)
                .await
            {
                Ok(count) => deleted += count,
                Err(error) if prune_canonical_contention(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(deleted)
    }

    async fn prune_job_row(
        &self,
        job: &str,
        guards: &str,
        cutoff_ms: i64,
        tail_cutoff: i64,
        has_foundation: bool,
    ) -> Result<u64, NotificationOutboxError> {
        let mut tx = self.begin_prune_transaction().await?;
        prelock_prune_ancestry(&mut tx, Some(job), has_foundation).await?;
        let lock = prune_row_lock(tx.driver());
        let mut rows = tx
            .query(
                &format!("SELECT job_id FROM notification_outbox WHERE job_id = ?{lock}"),
                crate::db_params![job],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Ok(0);
        }
        drop(rows);
        prelock_prune_ancestry(&mut tx, Some(job), has_foundation).await?;
        // Transfer exact completed-owner authority before deleting its last row.
        // This and DELETE roll back together; foreign stores remain conservative.
        crate::push_service::acknowledge_completed_outbox_tx(&mut tx, job).await?;
        let deleted = tx
            .execute(
                &format!("DELETE FROM notification_outbox WHERE job_id = ? AND {guards}"),
                crate::db_params![job, STATUS_PUBLISHED, STATUS_FAILED, cutoff_ms, tail_cutoff],
            )
            .await?;
        tx.commit().await?;
        Ok(deleted)
    }

    async fn begin_prune_transaction(
        &self,
    ) -> Result<crate::db::Transaction<'_>, NotificationOutboxError> {
        let timeout = std::time::Duration::from_millis(100);
        tokio::time::timeout(timeout, async {
            let mut tx = self.db.begin_immediate().await?;
            if tx.driver() == crate::db::DatabaseDriver::Postgres {
                // Every guard reread needs a fresh snapshot, even when the
                // connection's configured default is REPEATABLE READ.
                tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
                    .await?;
            }
            if !crate::ingress_substrate::set_local_transaction_timeouts(
                &mut tx,
                timeout,
                std::time::Duration::from_millis(250),
            )
            .await?
            {
                return Err(crate::ingress_uow::IngressUowError::TransactionBoundsUnproven.into());
            }
            Ok::<_, NotificationOutboxError>(tx)
        })
        .await
        .map_err(|_| crate::ingress_uow::IngressUowError::Timeout)?
    }

    async fn has_canonical_descendants(&self) -> Result<bool, NotificationOutboxError> {
        let mut tx = self.begin_prune_transaction().await?;
        let sql = if tx.driver() == crate::db::DatabaseDriver::Postgres {
            "SELECT 1 WHERE to_regclass('ingress_effect_descendants') IS NOT NULL"
        } else {
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ingress_effect_descendants'"
        };
        let mut rows = tx.query(sql, ()).await?;
        let exists = rows.next().await?.is_some();
        drop(rows);
        tx.commit().await?;
        Ok(exists)
    }
}

fn prune_row_lock(driver: crate::db::DatabaseDriver) -> &'static str {
    if driver == crate::db::DatabaseDriver::Postgres {
        " FOR UPDATE SKIP LOCKED"
    } else {
        ""
    }
}

async fn prelock_prune_ancestry(
    tx: &mut crate::db::Transaction<'_>,
    identity: Option<&str>,
    has_foundation: bool,
) -> Result<(), NotificationOutboxError> {
    if has_foundation {
        if let Some(id) = identity.and_then(|value| uuid::Uuid::parse_str(value).ok()) {
            crate::ingress_uow::EffectDescendantRepository::lock_all_nowait_raw(tx, id).await?;
        }
    }
    Ok(())
}

fn prune_canonical_contention(error: &NotificationOutboxError) -> bool {
    matches!(error, NotificationOutboxError::Ancestry(error)
        if error.retry_class() == crate::ingress_uow::DbRetryClass::CanonicalLockContention)
}

#[cfg(test)]
mod tests;
