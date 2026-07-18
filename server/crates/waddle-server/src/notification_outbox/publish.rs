//! Outbox claim/publish lifecycle: claiming due jobs, XEP-0357 publish
//! of claimed jobs, retry backoff, and terminal failure.

use super::*;

const MAX_OUTBOX_ATTEMPTS: i64 = 5;
const BASE_RETRY_DELAY_MS: i64 = 5_000;
const BASE_POLICY_RETRY_DELAY_MS: i64 = 60_000;
const MAX_RETRY_DELAY_MS: i64 = 300_000;
pub(super) const OUTBOX_CLAIM_TIMEOUT_MS: i64 = 300_000;

impl NotificationOutboxStore {
    pub async fn pending_outbox_jobs(
        &self,
    ) -> Result<Vec<NotificationOutboxJob>, NotificationOutboxError> {
        let mut rows = self
            .query(
                r#"
                SELECT job_id,
                       recipient_bare_jid,
                       push_service_jid,
                       node,
                       conversation_jid,
                       sender_jid,
                       sender_jids,
                       thread_id,
                       class,
                       message_count,
                       context_xml,
                       status,
                       attempt_count,
                       policy_error_count,
                       claim_token,
                       summary_sender_jid,
                       summary_body
                FROM notification_outbox
                WHERE status IN (?, ?)
                ORDER BY created_at_ms ASC, job_id ASC
                "#,
                crate::db_params![STATUS_QUEUED, STATUS_IN_PROGRESS],
            )
            .await?;
        let mut jobs = Vec::new();
        while let Some(row) = rows.next().await? {
            jobs.push(decode_outbox_job(&row)?);
        }
        Ok(jobs)
    }

    pub async fn claim_due_outbox_jobs(
        &self,
        batch_size: usize,
    ) -> Result<Vec<NotificationOutboxJob>, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 1_000);
        let now_ms = crate::time::now_ms();
        let stale_claimed_before_ms = now_ms.saturating_sub(OUTBOX_CLAIM_TIMEOUT_MS);
        let mut rows = self
            .query(
                r#"
                SELECT job_id,
                       recipient_bare_jid,
                       push_service_jid,
                       node,
                       conversation_jid,
                       sender_jid,
                       sender_jids,
                       thread_id,
                       class,
                       message_count,
                       context_xml,
                       status,
                       attempt_count,
                       policy_error_count,
                       claim_token,
                       summary_sender_jid,
                       summary_body
                FROM notification_outbox
                WHERE (
                    status = ?
                    AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)
                ) OR (
                    status = ?
                    AND claimed_at_ms IS NOT NULL
                    AND claimed_at_ms <= ?
                )
                ORDER BY created_at_ms ASC, job_id ASC
                LIMIT ?
                "#,
                crate::db_params![
                    STATUS_QUEUED,
                    now_ms,
                    STATUS_IN_PROGRESS,
                    stale_claimed_before_ms,
                    batch_size,
                ],
            )
            .await?;
        let mut selected = Vec::new();
        while let Some(row) = rows.next().await? {
            let job_id_raw: String = row.get(0)?;
            match decode_outbox_job(&row) {
                Ok(job) => selected.push(job),
                Err(error) => {
                    tracing::warn!(
                        job_id = %job_id_raw,
                        %error,
                        "failing malformed XEP-0357 notification outbox job fail-closed"
                    );
                    self.mark_malformed_outbox_job_failed(job_id_raw.as_str(), &error.to_string())
                        .await?;
                }
            }
        }

        let mut claimed = Vec::new();
        for job in selected {
            let claim_token = uuid::Uuid::new_v4().to_string();
            let affected = self
                .execute(
                    r#"
                    UPDATE notification_outbox
                    SET status = ?,
                        claimed_at_ms = ?,
                        claim_token = ?,
                        updated_at_ms = ?
                    WHERE job_id = ?
                      AND (
                        (
                            status = ?
                            AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)
                        ) OR (
                            status = ?
                            AND claimed_at_ms IS NOT NULL
                            AND claimed_at_ms <= ?
                        )
                      )
                    "#,
                    crate::db_params![
                        STATUS_IN_PROGRESS,
                        now_ms,
                        claim_token.as_str(),
                        now_ms,
                        job.job_id.as_str(),
                        STATUS_QUEUED,
                        now_ms,
                        STATUS_IN_PROGRESS,
                        stale_claimed_before_ms,
                    ],
                )
                .await?;
            if affected > 0 {
                claimed.push(NotificationOutboxJob {
                    status: NotificationOutboxStatus::InProgress,
                    claim_token: Some(claim_token),
                    ..job
                });
            }
        }
        Ok(claimed)
    }

    async fn mark_malformed_outbox_job_failed(
        &self,
        job_id: &str,
        error: &str,
    ) -> Result<(), NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        self.execute(
            r#"
            UPDATE notification_outbox
            SET status = ?,
                policy_error_count = 0,
                last_error = ?,
                next_attempt_at_ms = NULL,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status IN (?, ?)
            "#,
            crate::db_params![
                STATUS_FAILED,
                format!("malformed notification outbox job: {error}"),
                now_ms,
                job_id,
                STATUS_QUEUED,
                STATUS_IN_PROGRESS,
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn drain_due_outbox_jobs(
        &self,
        push_service: &crate::push_service::DatabasePushServiceStore,
        push_store: &dyn PushSubscriptionStore,
        inbox_storage: &dyn InboxStorage,
        blocking_storage: &dyn BlockingStorage,
        first_party_service_jid: &BareJid,
        batch_size: usize,
    ) -> Result<Vec<NotificationOutboxPublishOutcome>, NotificationOutboxError> {
        let jobs = self.claim_due_outbox_jobs(batch_size).await?;
        let mut outcomes = Vec::with_capacity(jobs.len());
        for job in jobs {
            let outcome = match self
                .publish_claimed_job(
                    &job,
                    push_service,
                    push_store,
                    inbox_storage,
                    blocking_storage,
                    first_party_service_jid,
                )
                .await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.retry_or_fail_outcome_for_claimed_job(&job, error.to_string())
                        .await?
                }
            };
            // #531 push-pipeline observability: bucket the typed
            // outcome into the parallel counter so a single drain
            // pass produces a histogram-like cardinality on the
            // metrics endpoint without per-job label explosion. The
            // Published / RetryScheduled / Failed arms are the
            // closed-set typed contract on
            // [`NotificationOutboxPublishOutcome`].
            match &outcome {
                NotificationOutboxPublishOutcome::Published { .. } => {
                    waddle_xmpp::telemetry::reliability::increment_push_outbox_published();
                }
                NotificationOutboxPublishOutcome::RetryScheduled { .. } => {
                    waddle_xmpp::telemetry::reliability::increment_push_outbox_retry_scheduled(
                        waddle_xmpp::telemetry::attributes::PushRetryReason::Unknown,
                    );
                }
                NotificationOutboxPublishOutcome::Failed { .. } => {
                    waddle_xmpp::telemetry::reliability::increment_push_outbox_dead_lettered();
                }
                NotificationOutboxPublishOutcome::Suppressed { .. } => {
                    waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                        SuppressedReason::UnreadZeroAtPublish.telemetry_reason(),
                    );
                }
            }
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }

    pub async fn publish_claimed_job(
        &self,
        job: &NotificationOutboxJob,
        push_service: &crate::push_service::DatabasePushServiceStore,
        push_store: &dyn PushSubscriptionStore,
        inbox_storage: &dyn InboxStorage,
        blocking_storage: &dyn BlockingStorage,
        first_party_service_jid: &BareJid,
    ) -> Result<NotificationOutboxPublishOutcome, NotificationOutboxError> {
        if job.push_service_jid() != first_party_service_jid {
            if self
                .mark_job_failed(
                    job,
                    "notification outbox job targets a non-first-party XEP-0357 Push Service",
                )
                .await?
            {
                return Ok(NotificationOutboxPublishOutcome::Failed {
                    job_id: job.job_id.clone(),
                });
            }
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        }

        match xep0191_blocks_notification_job(job, blocking_storage).await {
            Ok(true) => {
                if self
                    .mark_job_failed(job, "recipient blocked sender before XEP-0357 publish")
                    .await?
                {
                    return Ok(NotificationOutboxPublishOutcome::Failed {
                        job_id: job.job_id.clone(),
                    });
                }
                return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                    job_id: job.job_id.clone(),
                });
            }
            Ok(false) => {}
            Err(error) => {
                return self
                    .defer_claimed_job_without_attempt(
                        job,
                        format!("XEP-0191 blocklist load failed: {error}"),
                    )
                    .await;
            }
        }

        let registrations = push_store
            .get_for_user(&job.recipient_bare_jid.to_string())
            .await
            .map_err(|error| error.to_string());
        let registrations = match registrations {
            Ok(registrations) => registrations,
            Err(error) => {
                return self.retry_or_fail_outcome_for_claimed_job(job, error).await;
            }
        };
        let service = job.push_service_jid.to_string();
        let registration = registrations.into_iter().find(|registration| {
            registration.service_jid == service
                && registration.node.as_deref() == Some(job.node.as_str())
        });
        let Some(registration) = registration else {
            if self
                .mark_job_failed(job, "first-party XEP-0357 registration is no longer active")
                .await?
            {
                return Ok(NotificationOutboxPublishOutcome::Failed {
                    job_id: job.job_id.clone(),
                });
            }
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        };

        if !self.claimed_job_is_current(job).await? {
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        }

        let unread = current_unread_count_for_job(job, inbox_storage).await?;
        // #1126: the recipient reconnected and read the conversation
        // inside the notification window — an OS push now would be
        // for a message they already saw. Drop the job terminally.
        //
        // Suppress ONLY when a matching inbox entry EXISTS with
        // unread 0 (a positive "read it" signal). An ABSENT entry is
        // NOT the same thing: the inbox projection is written on a
        // separate, non-atomic path whose failures are swallowed
        // (`direct_inbox.rs` logs and drops), so "no entry" can mean
        // "projection lagged or failed", and suppressing there would
        // compound a partial failure into total notification loss.
        // Absent entries keep the pre-#1126 behavior: publish with
        // count 0.
        if unread == Some(0) {
            if self.suppress_claimed_job(job).await? {
                return Ok(NotificationOutboxPublishOutcome::Suppressed {
                    job_id: job.job_id.clone(),
                });
            }
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        }
        let message_count = unread.unwrap_or(0);
        let item = job.to_xep0357_pubsub_item_with_count(message_count);
        let push_service_jid = job.push_service_jid.to_string();
        match push_service
            .enqueue_registered_notification_from_user_server_with_publish_options(
                push_service_jid.as_str(),
                job.node.as_str(),
                &item,
                &job.recipient_bare_jid,
                registration.publish_options.as_ref(),
            )
            .await
        {
            Ok(result) => {
                if !self.mark_job_published(job).await? {
                    return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                        job_id: job.job_id.clone(),
                    });
                }
                Ok(NotificationOutboxPublishOutcome::Published {
                    job_id: job.job_id.clone(),
                    item_id: result.item_id().to_string(),
                })
            }
            Err(error) => {
                self.retry_or_fail_outcome_for_claimed_job(job, error.to_string())
                    .await
            }
        }
    }

    async fn claimed_job_is_current(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                r#"
                SELECT 1
                FROM notification_outbox
                WHERE job_id = ?
                  AND status = ?
                  AND claim_token = ?
                LIMIT 1
                "#,
                crate::db_params![
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    async fn retry_or_fail_outcome_for_claimed_job(
        &self,
        job: &NotificationOutboxJob,
        error: String,
    ) -> Result<NotificationOutboxPublishOutcome, NotificationOutboxError> {
        let Some(attempts) = self.schedule_retry_or_fail(job, error).await? else {
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        };
        if attempts >= MAX_OUTBOX_ATTEMPTS {
            Ok(NotificationOutboxPublishOutcome::Failed {
                job_id: job.job_id.clone(),
            })
        } else {
            Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            })
        }
    }

    async fn defer_claimed_job_without_attempt(
        &self,
        job: &NotificationOutboxJob,
        error: String,
    ) -> Result<NotificationOutboxPublishOutcome, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let next_policy_error_count = job.policy_error_count + 1;
        self.execute(
            r#"
            UPDATE notification_outbox
            SET status = ?,
                policy_error_count = ?,
                last_error = ?,
                next_attempt_at_ms = ?,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND claim_token = ?
            "#,
            crate::db_params![
                STATUS_QUEUED,
                next_policy_error_count,
                error,
                now_ms.saturating_add(policy_retry_delay_ms(next_policy_error_count)),
                now_ms,
                job.job_id.as_str(),
                STATUS_IN_PROGRESS,
                job.claim_token.as_deref(),
            ],
        )
        .await?;
        Ok(NotificationOutboxPublishOutcome::RetryScheduled {
            job_id: job.job_id.clone(),
        })
    }

    pub async fn mark_job_published(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<bool, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let affected = self
            .execute(
                r#"
            UPDATE notification_outbox
            SET status = ?,
                policy_error_count = 0,
                last_error = NULL,
                next_attempt_at_ms = NULL,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?,
                published_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND claim_token = ?
            "#,
                crate::db_params![
                    STATUS_PUBLISHED,
                    now_ms,
                    now_ms,
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        Ok(affected > 0)
    }

    /// #1126 terminal suppression: delete the claimed job outright.
    /// Suppression is not a publish (nothing was sent) and not a
    /// failure (nothing went wrong) — the job simply became moot, so
    /// no terminal row is kept. The audit trail is the labeled
    /// `unread_zero_at_publish` prometheus counter incremented by the
    /// drain loop. The `claim_token` predicate keeps the delete
    /// at-most-once: a stale claim deletes nothing and reports
    /// `false`, leaving the final state to the current claim-holder.
    async fn suppress_claimed_job(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<bool, NotificationOutboxError> {
        let affected = self
            .execute(
                r#"
            DELETE FROM notification_outbox
            WHERE job_id = ?
              AND status = ?
              AND claim_token = ?
            "#,
                crate::db_params![
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        Ok(affected > 0)
    }

    async fn mark_job_failed(
        &self,
        job: &NotificationOutboxJob,
        error: &str,
    ) -> Result<bool, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let affected = self
            .execute(
                r#"
            UPDATE notification_outbox
            SET status = ?,
                policy_error_count = 0,
                last_error = ?,
                next_attempt_at_ms = NULL,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND claim_token = ?
            "#,
                crate::db_params![
                    STATUS_FAILED,
                    error,
                    now_ms,
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        Ok(affected > 0)
    }

    pub async fn schedule_retry_or_fail(
        &self,
        job: &NotificationOutboxJob,
        error: String,
    ) -> Result<Option<i64>, NotificationOutboxError> {
        let next_attempt_count = job.attempt_count + 1;
        let now_ms = crate::time::now_ms();
        let (status, next_attempt_at_ms) = if next_attempt_count >= MAX_OUTBOX_ATTEMPTS {
            (STATUS_FAILED, None)
        } else {
            (
                STATUS_QUEUED,
                Some(now_ms.saturating_add(retry_delay_ms(next_attempt_count))),
            )
        };
        let affected = self
            .execute(
                r#"
            UPDATE notification_outbox
            SET status = ?,
                attempt_count = ?,
                policy_error_count = 0,
                last_error = ?,
                next_attempt_at_ms = ?,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND claim_token = ?
            "#,
                crate::db_params![
                    status,
                    next_attempt_count,
                    error,
                    next_attempt_at_ms,
                    now_ms,
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        if affected == 0 {
            return Ok(None);
        }
        Ok(Some(next_attempt_count))
    }
}

/// Current unread count for the job's conversation, or `None` when no
/// matching inbox entry exists. The distinction is load-bearing for
/// #1126: `Some(0)` is a positive "recipient read it" signal (the
/// entry exists, unread was cleared) and suppresses the push; `None`
/// means "no information" (inbox projection lagged or its write
/// failed) and must NOT suppress.
pub(super) async fn current_unread_count_for_job(
    job: &NotificationOutboxJob,
    inbox_storage: &dyn InboxStorage,
) -> Result<Option<u32>, NotificationOutboxError> {
    let entries = if job.thread_id.as_str().is_empty() {
        inbox_storage
            .list(job.recipient_bare_jid())
            .await
            .map_err(|error| NotificationOutboxError::Inbox(error.to_string()))?
    } else {
        inbox_storage
            .list_threads(job.recipient_bare_jid(), job.conversation_jid())
            .await
            .map_err(|error| NotificationOutboxError::Inbox(error.to_string()))?
    };
    Ok(entries
        .into_iter()
        .find(|entry| {
            entry.partner == *job.conversation_jid()
                && entry.thread_id.as_deref().unwrap_or("") == job.thread_id.as_str()
        })
        .map(|entry| entry.unread))
}

pub(super) fn retry_delay_ms(attempt_count: i64) -> i64 {
    let exponent = (attempt_count - 1).clamp(0, 10) as u32;
    apply_retry_jitter(
        BASE_RETRY_DELAY_MS
            .saturating_mul(2_i64.saturating_pow(exponent))
            .min(MAX_RETRY_DELAY_MS),
    )
}

pub(super) fn policy_retry_delay_ms(policy_error_count: i64) -> i64 {
    let exponent = (policy_error_count - 1).clamp(0, 10) as u32;
    apply_retry_jitter(
        BASE_POLICY_RETRY_DELAY_MS
            .saturating_mul(2_i64.saturating_pow(exponent))
            .min(MAX_RETRY_DELAY_MS),
    )
}

/// #1126: randomize a retry delay by ±25% so a fleet of jobs failed
/// by one outage does not drain in synchronized waves — deterministic
/// backoff re-aligns every retry onto the same instant, re-creating
/// the thundering herd on each cycle.
pub(super) fn apply_retry_jitter(delay_ms: i64) -> i64 {
    use rand::RngExt as _;
    let factor: f64 = rand::rng().random_range(0.75..=1.25);
    // `as` clamps on overflow and the factor keeps the product within
    // 1.25x of an i64 that started life as a bounded delay constant.
    ((delay_ms as f64) * factor) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification_outbox::test_support::*;

    // #1126: exponential backoff carries ±25% jitter so retries after
    // an outage do not drain in synchronized waves.
    #[test]
    fn retry_delays_are_jittered_within_bounds() {
        let mut distinct = std::collections::HashSet::new();
        for _ in 0..200 {
            let delay = retry_delay_ms(1);
            assert!(
                (3_750..=6_250).contains(&delay),
                "attempt-1 jittered delay {delay} outside ±25% of {BASE_RETRY_DELAY_MS}"
            );
            distinct.insert(delay);

            let policy_delay = policy_retry_delay_ms(1);
            assert!(
                (45_000..=75_000).contains(&policy_delay),
                "policy jittered delay {policy_delay} outside ±25% of {BASE_POLICY_RETRY_DELAY_MS}"
            );
        }
        assert!(
            distinct.len() > 1,
            "200 samples produced a single delay — backoff is not jittered"
        );
    }

    // The jitter factor must respect the MAX cap direction too: a
    // capped delay may exceed the cap by at most +25%.
    #[test]
    fn jittered_delay_never_exceeds_cap_plus_jitter() {
        for attempt in 1..=12 {
            let delay = retry_delay_ms(attempt);
            assert!(delay <= (MAX_RETRY_DELAY_MS as f64 * 1.25) as i64);
            assert!(delay >= (BASE_RETRY_DELAY_MS as f64 * 0.75) as i64);
        }
    }

    #[tokio::test]
    async fn xep0357_publish_count_is_derived_from_current_inbox_unread() {
        let store = store().await;
        let recipient = bare("alice@example.com");
        let conversation = bare("bob@example.com");
        enqueue_jobs_for_test(&store, &candidate("archive-1"), &[target()]).await;
        let claimed = store
            .claim_due_outbox_jobs(16)
            .await
            .expect("claim")
            .into_iter()
            .next()
            .expect("claimed job");
        assert_eq!(claimed.message_count(), 1);
        let inbox = inbox_with_unread(&recipient, &conversation, 3).await;

        let current_count = current_unread_count_for_job(&claimed, &inbox)
            .await
            .expect("current unread")
            .expect("inbox entry present");
        let item = claimed.to_xep0357_pubsub_item_with_count(current_count);
        let payload = item.payload.expect("payload");
        let summary = payload
            .children()
            .find(|child| child.is("x", NS_DATA_FORMS))
            .expect("summary form");

        assert!(summary.children().any(|field| {
            field.is("field", NS_DATA_FORMS)
                && field.attr("var") == Some("message-count")
                && field
                    .children()
                    .any(|value| value.is("value", NS_DATA_FORMS) && value.text() == "3")
        }));
    }

    #[tokio::test]
    async fn stale_claim_cannot_mark_reclaimed_outbox_job_published() {
        let store = store().await;
        let target = target();
        enqueue_jobs_for_test(
            &store,
            &candidate("archive-1"),
            std::slice::from_ref(&target),
        )
        .await;
        let (stale_claim, fresh_claim) = reclaim_stale_job(&store).await;

        assert!(
            !store
                .mark_job_published(&stale_claim)
                .await
                .expect("stale publish mark should not fail"),
            "stale worker must not complete a job after another worker reclaimed it"
        );
        let pending = store.pending_outbox_jobs().await.expect("pending jobs");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status(), NotificationOutboxStatus::InProgress);
        assert_eq!(pending[0].claim_token(), fresh_claim.claim_token());

        assert!(store
            .mark_job_published(&fresh_claim)
            .await
            .expect("fresh publish mark should succeed"));
        assert!(store
            .pending_outbox_jobs()
            .await
            .expect("pending after fresh mark")
            .is_empty());
    }
}
