//! Retention pruning of published/failed jobs and outboxed candidates.

use super::{
    NotificationOutboxError, NotificationOutboxPruneOutcome, NotificationOutboxStore, Row,
    STATUS_FAILED, STATUS_PUBLISHED,
};
#[cfg(feature = "test-support")]
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
use crate::db::DatabaseDriver;
use crate::db::Transaction;
use crate::ingress_substrate::ALIAS_RETENTION;
use crate::ingress_substrate::{DefinerApiCandidateContext, PreparedAttestedReceiptWriter};
use crate::ingress_uow::EffectDescendantRepository;
use crate::ingress_uow::IngressUowError;
#[cfg(feature = "test-support")]
use crate::ingress_uow::RelationGuardCapability;

#[derive(Clone, Copy)]
struct PruneMode<'a> {
    #[cfg(feature = "test-support")]
    candidate: Option<(
        &'a AttestedPostgresWriterCapability,
        &'a RelationGuardCapability,
    )>,
    marker: std::marker::PhantomData<&'a ()>,
}

impl PruneMode<'_> {
    fn ordinary() -> Self {
        Self {
            #[cfg(feature = "test-support")]
            candidate: None,
            marker: std::marker::PhantomData,
        }
    }
}

impl NotificationOutboxStore {
    pub async fn prune_completed_before(
        &self,
        cutoff_ms: i64,
        batch_size: usize,
    ) -> Result<NotificationOutboxPruneOutcome, NotificationOutboxError> {
        self.prune_completed_before_mode(cutoff_ms, batch_size, PruneMode::ordinary())
            .await
    }

    #[cfg(feature = "test-support")]
    pub(crate) async fn prune_completed_before_with_candidate(
        &self,
        cutoff_ms: i64,
        batch_size: usize,
        capability: &AttestedPostgresWriterCapability,
        relation_guard: &RelationGuardCapability,
    ) -> Result<NotificationOutboxPruneOutcome, NotificationOutboxError> {
        self.prune_completed_before_mode(
            cutoff_ms,
            batch_size,
            PruneMode {
                candidate: Some((capability, relation_guard)),
                marker: std::marker::PhantomData,
            },
        )
        .await
    }

    async fn prune_completed_before_mode(
        &self,
        cutoff_ms: i64,
        batch_size: usize,
        mode: PruneMode<'_>,
    ) -> Result<NotificationOutboxPruneOutcome, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 10_000);
        let tail_cutoff =
            cutoff_ms.min(crate::time::now_ms().saturating_sub(ALIAS_RETENTION.num_milliseconds()));
        let has_foundation = self.has_canonical_descendants(mode).await?;
        let candidates_deleted = self
            .prune_outboxed_candidates_before(
                cutoff_ms,
                tail_cutoff,
                batch_size,
                has_foundation,
                mode,
            )
            .await?;
        let jobs_deleted = self
            .prune_jobs_before(cutoff_ms, tail_cutoff, batch_size, has_foundation, mode)
            .await?;
        let (mut tx, _prepared) = self.begin_prune_transaction(mode).await?;
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
        mode: PruneMode<'_>,
    ) -> Result<u64, NotificationOutboxError> {
        let (mut tx, prepared) = self.begin_prune_transaction(mode).await?;
        let api = prepared
            .as_ref()
            .and_then(DefinerApiCandidateContext::from_prepared);
        let foundation_guard = if has_foundation {
            if let Some(api) = api.as_ref() {
                format!(
                    "AND NOT {}(notification_candidates.delivery_id, FALSE)",
                    api.qualified_function("waddle_ingress_descendant_exists")
                )
            } else {
                "AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = notification_candidates.delivery_id)".to_owned()
            }
        } else {
            String::new()
        };
        let guards = format!(
            "outboxed_at_ms IS NOT NULL AND quarantined_at_ms IS NULL AND outboxed_at_ms < ? {foundation_guard} AND NOT EXISTS (SELECT 1 FROM notification_outbox_lineage AS lineage WHERE lineage.candidate_delivery_id = notification_candidates.delivery_id AND (lineage.settled_at_ms IS NULL OR lineage.settled_at_ms > ?))"
        );
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
                .prune_candidate_row(&row, &guards, cutoff_ms, tail_cutoff, has_foundation, mode)
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
        mode: PruneMode<'_>,
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
        let (mut tx, prepared) = self.begin_prune_transaction(mode).await?;
        let api = prepared
            .as_ref()
            .and_then(DefinerApiCandidateContext::from_prepared);
        prelock_prune_ancestry(&mut tx, api.as_ref(), delivery.as_deref(), has_foundation).await?;
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
        prelock_prune_ancestry(
            &mut tx,
            api.as_ref(),
            current_delivery.as_deref(),
            has_foundation,
        )
        .await?;
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
        mode: PruneMode<'_>,
    ) -> Result<u64, NotificationOutboxError> {
        let (mut tx, prepared) = self.begin_prune_transaction(mode).await?;
        let api = prepared
            .as_ref()
            .and_then(DefinerApiCandidateContext::from_prepared);
        let foundation_guard = if has_foundation {
            if let Some(api) = api.as_ref() {
                format!(
                    "AND NOT {}(notification_outbox.job_id, FALSE)",
                    api.qualified_function("waddle_ingress_descendant_exists")
                )
            } else {
                "AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = notification_outbox.job_id)".to_owned()
            }
        } else {
            String::new()
        };
        let guards = format!(
            "status IN (?, ?) AND (status != '{STATUS_FAILED}' OR queue_acceptance_may_exist = 0) AND updated_at_ms < ? {foundation_guard} AND NOT EXISTS (SELECT 1 FROM notification_outbox_lineage AS lineage WHERE lineage.job_id = notification_outbox.job_id AND (lineage.settled_at_ms IS NULL OR lineage.settled_at_ms > ?))"
        );
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
                .prune_job_row(&job, &guards, cutoff_ms, tail_cutoff, has_foundation, mode)
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
        mode: PruneMode<'_>,
    ) -> Result<u64, NotificationOutboxError> {
        let (mut tx, prepared) = self.begin_prune_transaction(mode).await?;
        let api = prepared
            .as_ref()
            .and_then(DefinerApiCandidateContext::from_prepared);
        prelock_prune_ancestry(&mut tx, api.as_ref(), Some(job), has_foundation).await?;
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
        prelock_prune_ancestry(&mut tx, api.as_ref(), Some(job), has_foundation).await?;
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
        mode: PruneMode<'_>,
    ) -> Result<(Transaction<'_>, Option<PreparedAttestedReceiptWriter>), NotificationOutboxError>
    {
        let timeout = std::time::Duration::from_millis(100);
        tokio::time::timeout(timeout, async {
            #[cfg(feature = "test-support")]
            if let Some((capability, relation_guard)) = mode.candidate {
                let (tx, prepared) =
                    crate::ingress_uow::begin_raw_ingress_writer_with_definer_api_candidate(
                        &self.db,
                        capability,
                        relation_guard,
                    )
                    .await?;
                return Ok::<_, NotificationOutboxError>((tx, Some(prepared)));
            }
            #[cfg(not(feature = "test-support"))]
            let _ = mode;
            let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
            crate::ingress_uow::bound_raw_transaction(&mut tx).await?;
            Ok::<_, NotificationOutboxError>((tx, None))
        })
        .await
        .map_err(|_| IngressUowError::Timeout)?
    }

    async fn has_canonical_descendants(
        &self,
        mode: PruneMode<'_>,
    ) -> Result<bool, NotificationOutboxError> {
        let (mut tx, prepared) = self.begin_prune_transaction(mode).await?;
        let candidate_schema = prepared
            .as_ref()
            .and_then(DefinerApiCandidateContext::from_prepared)
            .map(|api| api.schema().to_owned());
        let sql = if let Some(schema) = candidate_schema {
            let relation = format!(
                "{}.ingress_effect_descendants",
                crate::ingress_substrate::quote_postgres_identifier(&schema)
            );
            let mut rows = tx
                .query(
                    "SELECT 1 WHERE to_regclass(?) IS NOT NULL",
                    crate::db_params![relation],
                )
                .await?;
            let exists = rows.next().await?.is_some();
            drop(rows);
            tx.commit().await?;
            return Ok(exists);
        } else if tx.driver() == DatabaseDriver::Postgres {
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

fn prune_row_lock(driver: DatabaseDriver) -> &'static str {
    if driver == DatabaseDriver::Postgres {
        " FOR UPDATE SKIP LOCKED"
    } else {
        ""
    }
}

async fn prelock_prune_ancestry(
    tx: &mut Transaction<'_>,
    api: Option<&DefinerApiCandidateContext<'_>>,
    identity: Option<&str>,
    has_foundation: bool,
) -> Result<(), NotificationOutboxError> {
    if has_foundation {
        if let Some(id) = identity.and_then(|value| uuid::Uuid::parse_str(value).ok()) {
            if let Some(api) = api {
                EffectDescendantRepository::lock_all_with_candidate_api(tx, api, id).await?;
            } else {
                EffectDescendantRepository::lock_all_nowait_raw(tx, id).await?;
            }
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
