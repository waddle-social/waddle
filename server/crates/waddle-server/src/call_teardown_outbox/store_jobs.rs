use super::store::{
    select_columns, OWNERSHIP_RETRY_DELAY_MS, STATUS_DONE, STATUS_FAILED, STATUS_IN_PROGRESS,
    STATUS_QUEUED,
};
use super::store_rows::decode_job;
use super::{
    CallTeardownJob, CallTeardownLastError, CallTeardownOutboxError, CallTeardownOutboxStore,
    CallTeardownRetryOutcome, CallTeardownRetryReason, CallTeardownStatus, ClaimToken,
    CLAIM_TIMEOUT_MS, MAX_ATTEMPTS,
};

impl CallTeardownOutboxStore {
    pub async fn claim_due(
        &self,
        batch_size: usize,
    ) -> Result<Vec<CallTeardownJob>, CallTeardownOutboxError> {
        self.claim_due_at(batch_size, crate::time::now_ms()).await
    }

    pub(super) async fn claim_due_at(
        &self,
        batch_size: usize,
        now_ms: i64,
    ) -> Result<Vec<CallTeardownJob>, CallTeardownOutboxError> {
        let batch_size = batch_size.clamp(1, 1_000);
        let stale_before_ms = now_ms.saturating_sub(CLAIM_TIMEOUT_MS);
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                &format!(
                    "{} WHERE (\
                        status = ? AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)\
                     ) OR (\
                        status = ? AND claimed_at_ms IS NOT NULL AND claimed_at_ms <= ?\
                     ) ORDER BY created_at_ms ASC, intent_id ASC LIMIT ?",
                    select_columns()
                ),
                crate::db_params![
                    STATUS_QUEUED,
                    now_ms,
                    STATUS_IN_PROGRESS,
                    stale_before_ms,
                    batch_size,
                ],
            )
            .await?;
        let mut selected = Vec::new();
        while let Some(row) = rows.next().await? {
            selected.push(decode_job(&row)?);
        }
        drop(rows);
        drop(connection);

        let mut claimed = Vec::with_capacity(selected.len());
        for mut job in selected {
            let claim_token = ClaimToken::new();
            let connection = self.db.guard().await?;
            let affected = connection
                .execute(
                    "UPDATE call_teardown_outbox \
                     SET status = ?, claimed_at_ms = ?, claim_token = ?, updated_at_ms = ? \
                     WHERE intent_id = ? AND (\
                        (status = ? AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)) \
                        OR (status = ? AND claimed_at_ms IS NOT NULL AND claimed_at_ms <= ?)\
                     )",
                    crate::db_params![
                        STATUS_IN_PROGRESS,
                        now_ms,
                        claim_token.as_str(),
                        now_ms,
                        job.intent_id.as_str(),
                        STATUS_QUEUED,
                        now_ms,
                        STATUS_IN_PROGRESS,
                        stale_before_ms,
                    ],
                )
                .await?;
            if affected == 1 {
                job.status = CallTeardownStatus::InProgress;
                job.claim_token = Some(claim_token);
                claimed.push(job);
            }
        }
        Ok(claimed)
    }

    pub async fn mark_done(&self, job: &CallTeardownJob) -> Result<bool, CallTeardownOutboxError> {
        self.mark_done_at(job, crate::time::now_ms()).await
    }

    pub(super) async fn mark_done_at(
        &self,
        job: &CallTeardownJob,
        now_ms: i64,
    ) -> Result<bool, CallTeardownOutboxError> {
        let connection = self.db.guard().await?;
        let affected = connection
            .execute(
                "UPDATE call_teardown_outbox \
                 SET status = ?, last_error = NULL, next_attempt_at_ms = NULL, \
                     claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ? \
                 WHERE intent_id = ? AND status = ? AND claim_token = ?",
                crate::db_params![
                    STATUS_DONE,
                    now_ms,
                    job.intent_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_ref().map(ClaimToken::as_str),
                ],
            )
            .await?;
        Ok(affected == 1)
    }

    /// Releases a claim without counting an execution attempt. The drain uses
    /// this when another node owns the intent's clustered room/user scope or
    /// when claim lookup itself is unavailable.
    pub async fn release_claim(
        &self,
        job: &CallTeardownJob,
    ) -> Result<bool, CallTeardownOutboxError> {
        self.release_claim_at(job, crate::time::now_ms()).await
    }

    pub(super) async fn release_claim_at(
        &self,
        job: &CallTeardownJob,
        now_ms: i64,
    ) -> Result<bool, CallTeardownOutboxError> {
        let next_attempt_at_ms = now_ms.saturating_add(OWNERSHIP_RETRY_DELAY_MS);
        let connection = self.db.guard().await?;
        let affected = connection
            .execute(
                "UPDATE call_teardown_outbox \
                 SET status = ?, next_attempt_at_ms = ?, claimed_at_ms = NULL, \
                     claim_token = NULL, updated_at_ms = ? \
                 WHERE intent_id = ? AND status = ? AND claim_token = ?",
                crate::db_params![
                    STATUS_QUEUED,
                    next_attempt_at_ms,
                    now_ms,
                    job.intent_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_ref().map(ClaimToken::as_str),
                ],
            )
            .await?;
        Ok(affected == 1)
    }

    pub async fn fail_claim(
        &self,
        job: &CallTeardownJob,
        error: CallTeardownLastError,
    ) -> Result<bool, CallTeardownOutboxError> {
        self.fail_claim_at(job, error, crate::time::now_ms()).await
    }

    pub(super) async fn fail_claim_at(
        &self,
        job: &CallTeardownJob,
        error: CallTeardownLastError,
        now_ms: i64,
    ) -> Result<bool, CallTeardownOutboxError> {
        let connection = self.db.guard().await?;
        let affected = connection
            .execute(
                "UPDATE call_teardown_outbox \
                 SET status = ?, last_error = ?, next_attempt_at_ms = NULL, \
                     claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ? \
                 WHERE intent_id = ? AND status = ? AND claim_token = ?",
                crate::db_params![
                    STATUS_FAILED,
                    error.as_db_value(),
                    now_ms,
                    job.intent_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_ref().map(ClaimToken::as_str),
                ],
            )
            .await?;
        Ok(affected == 1)
    }

    pub async fn retry_or_fail(
        &self,
        job: &CallTeardownJob,
        error: CallTeardownRetryReason,
    ) -> Result<CallTeardownRetryOutcome, CallTeardownOutboxError> {
        self.retry_or_fail_at(job, error, crate::time::now_ms())
            .await
    }

    pub(super) async fn retry_or_fail_at(
        &self,
        job: &CallTeardownJob,
        error: CallTeardownRetryReason,
        now_ms: i64,
    ) -> Result<CallTeardownRetryOutcome, CallTeardownOutboxError> {
        let attempt_count = job.attempt_count.saturating_add(1);
        let failed = attempt_count >= MAX_ATTEMPTS;
        let status = if failed { STATUS_FAILED } else { STATUS_QUEUED };
        let next_attempt_at_ms =
            (!failed).then(|| now_ms.saturating_add(super::retry_delay_ms(attempt_count)));
        let connection = self.db.guard().await?;
        let affected = connection
            .execute(
                "UPDATE call_teardown_outbox \
                 SET status = ?, attempt_count = ?, last_error = ?, \
                     next_attempt_at_ms = ?, claimed_at_ms = NULL, claim_token = NULL, \
                     updated_at_ms = ? \
                 WHERE intent_id = ? AND status = ? AND claim_token = ?",
                crate::db_params![
                    status,
                    attempt_count,
                    error.as_db_value(),
                    next_attempt_at_ms,
                    now_ms,
                    job.intent_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_ref().map(ClaimToken::as_str),
                ],
            )
            .await?;
        if affected == 0 {
            return Ok(CallTeardownRetryOutcome::ClaimLost);
        }
        if failed {
            Ok(CallTeardownRetryOutcome::Failed { attempt_count })
        } else {
            Ok(CallTeardownRetryOutcome::Requeued { attempt_count })
        }
    }
}
