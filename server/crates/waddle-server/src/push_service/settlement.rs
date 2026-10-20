//! Bounded completion of known failures deferred by canonical lock contention.
//! This never resumes a provider send or settles an uncertain acceptance.

use super::publish_jobs::{
    acknowledge_completed_outbox_tx, get_publish_job_tx, has_notification_outbox_frozen_schema_tx,
    lock_notification_ancestry_typed_tx, pending_notification_custody_sql,
    read_publish_job_claim_token_tx, settle_terminal_notification_ancestry_tx,
    NotificationAncestryLockError,
};
use super::store::{lock_node_tx, lock_owner_tx};
use super::DatabasePushServiceStore;
use waddle_xmpp::XmppError;

impl DatabasePushServiceStore {
    /// Rotate failed known jobs across bounded batches, including past a
    /// contended parent. A failed row's idle retry timestamp is only a probe
    /// cursor; its status excludes it from the provider scheduler.
    pub(super) async fn reconcile_failed_publish_job_custody(
        &self,
        limit: usize,
    ) -> Result<u64, XmppError> {
        let now_ms = crate::time::now_ms();
        let mut tx = self.db.begin_immediate().await.map_err(internal)?;
        let pending = pending_notification_custody_sql(&mut tx).await?;
        let lock = if tx.driver() == crate::db::DatabaseDriver::Postgres {
            " FOR UPDATE SKIP LOCKED"
        } else {
            ""
        };
        let mut rows = tx.query(
            &format!("SELECT job_id FROM push_publish_jobs WHERE status = 'failed' AND uncertain_send = 0 AND terminal_disposition IS NULL AND ancestry_job_id IS NOT NULL AND ({pending}) ORDER BY COALESCE(next_retry_at_ms, 0), job_id LIMIT ?{lock}"),
            crate::db_params![limit.clamp(1, 1_000)],
        ).await.map_err(internal)?;
        let mut jobs = Vec::new();
        while let Some(row) = rows.next().await.map_err(internal)? {
            jobs.push(row.get::<String>(0).map_err(internal)?);
        }
        drop(rows);
        // Hold only scheduler row locks during rotation and release them
        // before requesting ancestry. Rows held by another operation are skipped.
        for id in &jobs {
            tx.execute(
                "UPDATE push_publish_jobs SET next_retry_at_ms = CASE WHEN next_retry_at_ms >= 9223372036854775807 THEN next_retry_at_ms WHEN next_retry_at_ms >= ? THEN next_retry_at_ms + 1 ELSE ? END WHERE job_id = ? AND status = 'failed' AND uncertain_send = 0 AND terminal_disposition IS NULL",
                crate::db_params![now_ms, now_ms, id.as_str()],
            ).await.map_err(internal)?;
        }
        tx.commit().await.map_err(internal)?;

        let mut settled = 0;
        for id in jobs {
            match self.settle_known_failure(&id, now_ms).await {
                Ok(true) => settled += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(job_id = id, %error, "known push failure settlement deferred");
                }
            }
        }
        Ok(settled)
    }

    async fn settle_known_failure(&self, id: &str, now_ms: i64) -> Result<bool, XmppError> {
        let mut tx = self.db.begin_immediate().await.map_err(internal)?;
        let Some(job) = get_publish_job_tx(&mut tx, id).await? else {
            return Ok(false);
        };
        match lock_notification_ancestry_typed_tx(&mut tx, &job).await {
            Ok(()) => {}
            Err(NotificationAncestryLockError::Contended) => return Ok(false),
            Err(NotificationAncestryLockError::Failed(error)) => return Err(error),
        }
        lock_owner_tx(&mut tx, job.owner_bare_jid(), now_ms).await?;
        lock_node_tx(&mut tx, job.node(), now_ms).await?;
        // Failed rows have no claim token, but the read still acquires their
        // NOWAIT row lock and rejects a writer racing the discovery pass.
        read_publish_job_claim_token_tx(&mut tx, id).await?;
        let pending = pending_notification_custody_sql(&mut tx).await?;
        let changed = tx.execute(
            &format!("UPDATE push_publish_jobs SET next_retry_at_ms = next_retry_at_ms WHERE job_id = ? AND status = 'failed' AND uncertain_send = 0 AND terminal_disposition IS NULL AND ({pending})"),
            crate::db_params![id],
        ).await.map_err(internal)?;
        if changed == 0 {
            return Ok(false);
        }
        // Catch a parent attached after the initial discovery without creating
        // an inverted wait. The durable failed/known row remains fenced.
        lock_notification_ancestry_typed_tx(&mut tx, &job)
            .await
            .map_err(|error| match error {
                NotificationAncestryLockError::Contended => {
                    XmppError::internal("notification ancestry unavailable")
                }
                NotificationAncestryLockError::Failed(error) => error,
            })?;
        settle_terminal_notification_ancestry_tx(&mut tx, &job).await?;
        if has_notification_outbox_frozen_schema_tx(&mut tx).await? {
            if let Some(ancestry) = job.ancestry_job_id() {
                acknowledge_completed_outbox_tx(&mut tx, &ancestry.to_string())
                    .await
                    .map_err(internal)?;
            }
        }
        tx.commit().await.map_err(internal)?;
        tracing::info!(
            job_id = id,
            disposition = "known_failure_settled",
            "push custody transition"
        );
        Ok(true)
    }
}

fn internal(error: impl std::fmt::Display) -> XmppError {
    XmppError::internal(error.to_string())
}
