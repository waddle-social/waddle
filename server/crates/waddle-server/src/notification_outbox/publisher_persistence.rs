//! Fixed persistence operations for the live notification publisher.
//!
//! Each operation retains the original transaction boundary, database handle,
//! and admission lane. Shared transaction helpers run in their caller's transaction.

use super::publish::{
    policy_retry_delay_ms, record_failure_transition, retry_delay_ms, uncertain_retry_delay_ms,
    PublishFailure, PublishFailureClass, MAX_OUTBOX_ATTEMPTS,
};
use super::{
    decode_outbox_job, BareJid, Element, InboxStorage, NotificationOutboxError,
    NotificationOutboxJob, NotificationOutboxStatus, NotificationOutboxStore, PubSubItem, Row,
    STATUS_FAILED, STATUS_IN_PROGRESS, STATUS_PUBLISHED, STATUS_QUEUED,
};
use crate::db::{DatabaseDriver, Transaction};
use crate::ingress_substrate::DefinerApiCandidateContext;
use crate::ingress_uow::EffectDescendantRepository;
use crate::push_service::CanonicalNotificationTerminalProof;

pub(super) struct PublisherTransition {
    pub(super) changed: u64,
    pub(super) uncertain: bool,
}

pub(super) struct PublisherRetryTransition {
    pub(super) terminal: bool,
    pub(super) uncertain: bool,
}

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
                       summary_body,
                       approved_payload_xml,
                       approved_publish_options_xml,
                       queue_acceptance_may_exist
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

    pub async fn mark_job_published(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<bool, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
        let affected = tx
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
        if affected > 0 {
            crate::push_service::acknowledge_completed_outbox_tx(&mut tx, job.job_id.as_str())
                .await?;
        }
        tx.commit().await?;
        Ok(affected > 0)
    }

    pub(super) async fn persist_queue_acceptance(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<Option<bool>, NotificationOutboxError> {
        let mut tx = self.db.begin_immediate().await?;
        // A no-op UPDATE returns the previous marker while locking this row on
        // both engines. Read the durable value, not a potentially reused snapshot.
        let mut rows = tx.query(
            "UPDATE notification_outbox SET queue_acceptance_may_exist = queue_acceptance_may_exist WHERE job_id = ? AND status = ? AND claim_token = ? RETURNING queue_acceptance_may_exist",
            crate::db_params![job.job_id.as_str(), STATUS_IN_PROGRESS, job.claim_token.as_deref()],
        ).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let prior_acceptance_may_exist = row.get::<i64>(0)? != 0;
        if prior_acceptance_may_exist && !job.queue_acceptance_may_exist {
            // Another invocation of this same first-attempt claim already began.
            // Only that invocation may clear its newly created uncertainty.
            return Ok(None);
        }
        drop(rows);
        tx.execute(
            "UPDATE notification_outbox SET queue_acceptance_may_exist = 1 WHERE job_id = ? AND status = ? AND claim_token = ?",
            crate::db_params![job.job_id.as_str(), STATUS_IN_PROGRESS, job.claim_token.as_deref()],
        ).await?;
        tx.commit().await?;
        Ok(Some(prior_acceptance_may_exist))
    }

    pub(super) async fn persist_known_queue_refusal(
        &self,
        job: &NotificationOutboxJob,
        prior_acceptance_may_exist: bool,
    ) -> Result<(), NotificationOutboxError> {
        if !prior_acceptance_may_exist {
            self.execute(
                "UPDATE notification_outbox SET queue_acceptance_may_exist = 0 WHERE job_id = ? AND status = ? AND claim_token = ?",
                crate::db_params![job.job_id.as_str(), STATUS_IN_PROGRESS, job.claim_token.as_deref()],
            ).await?;
        }
        Ok(())
    }

    pub(super) async fn persist_approved_item(
        &self,
        job: &NotificationOutboxJob,
        item: &PubSubItem,
        publish_options: Option<&Element>,
    ) -> Result<Option<(PubSubItem, Option<Element>)>, NotificationOutboxError> {
        let mut tx = self.db.begin_immediate().await?;
        let changed = tx.execute(
            "UPDATE notification_outbox SET approved_payload_xml = COALESCE(approved_payload_xml, ?), approved_publish_options_xml = CASE WHEN approved_payload_xml IS NULL THEN ? ELSE approved_publish_options_xml END WHERE job_id = ? AND status = ? AND claim_token = ?",
            crate::db_params![item.payload.as_ref().map(String::from), publish_options.map(String::from),
                job.job_id.as_str(), STATUS_IN_PROGRESS, job.claim_token.as_deref()],
        ).await?;
        if changed == 0 {
            return Ok(None);
        }
        let mut rows = tx.query("SELECT approved_payload_xml, approved_publish_options_xml FROM notification_outbox WHERE job_id = ?",
            crate::db_params![job.job_id.as_str()]).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let payload = row
            .get::<String>(0)?
            .parse::<Element>()
            .map_err(|_| NotificationOutboxError::InvalidApprovedPayload)?;
        let options = row
            .get::<Option<String>>(1)?
            .map(|raw| raw.parse::<Element>())
            .transpose()
            .map_err(|_| NotificationOutboxError::InvalidApprovedPayload)?;
        tx.commit().await?;
        Ok(Some((
            PubSubItem::new(Some(job.job_id.as_str().to_string()), Some(payload)),
            options,
        )))
    }

    pub(super) async fn persist_claim_currency_check(
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

    pub(super) async fn persist_claimed_job_suppression(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<bool, NotificationOutboxError> {
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
        let affected = tx
            .execute(
                r#"
            DELETE FROM notification_outbox
            WHERE job_id = ?
              AND status = ?
              AND claim_token = ?
              AND queue_acceptance_may_exist = 0
            "#,
                crate::db_params![
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        if affected > 0 {
            settle_outbox_if_unowned_tx(&mut tx, job.job_id.as_str()).await?;
        }
        tx.commit().await?;
        Ok(affected > 0)
    }

    pub(super) async fn persist_due_outbox_selection(
        &self,
        batch_size: usize,
        now_ms: i64,
        stale_claimed_before_ms: i64,
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
                       summary_body,
                       approved_payload_xml,
                       approved_publish_options_xml,
                       queue_acceptance_may_exist
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
            match decode_outbox_job(&row) {
                Ok(job) => selected.push(job),
                Err(_) => {
                    tracing::warn!(
                        "quarantining malformed XEP-0357 notification outbox job fail-closed"
                    );
                    self.mark_malformed_outbox_job_failed(&row, now_ms, stale_claimed_before_ms)
                        .await?;
                }
            }
        }

        Ok(selected)
    }

    pub(super) async fn persist_outbox_claim(
        &self,
        job: &NotificationOutboxJob,
        now_ms: i64,
        claim_token: String,
    ) -> Result<Option<NotificationOutboxJob>, NotificationOutboxError> {
        let affected = self
            .execute(
                r#"
                    UPDATE notification_outbox
                    SET status = ?,
                        claimed_at_ms = ?,
                        claim_token = ?,
                        updated_at_ms = ?
                    WHERE job_id = ?
                      AND status = ?
                      AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)
                    "#,
                crate::db_params![
                    STATUS_IN_PROGRESS,
                    now_ms,
                    claim_token.as_str(),
                    now_ms,
                    job.job_id.as_str(),
                    STATUS_QUEUED,
                    now_ms,
                ],
            )
            .await?;
        if affected > 0 {
            // A coalescing transaction may have committed after the
            // candidate SELECT but before this claim won. Freeze the
            // current claimed row, never that earlier payload snapshot.
            let mut rows = self.query(
                    "SELECT job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, thread_id, class, message_count, context_xml, status, attempt_count, policy_error_count, claim_token, summary_sender_jid, summary_body, approved_payload_xml, approved_publish_options_xml, queue_acceptance_may_exist FROM notification_outbox WHERE job_id = ? AND status = ? AND claim_token = ?",
                    crate::db_params![job.job_id.as_str(), STATUS_IN_PROGRESS, claim_token],
                ).await?;
            if let Some(row) = rows.next().await? {
                return Ok(Some(decode_outbox_job(&row)?));
            }
        }
        Ok(None)
    }

    async fn mark_malformed_outbox_job_failed(
        &self,
        selected: &Row,
        now_ms: i64,
        stale_claimed_before_ms: i64,
    ) -> Result<(), NotificationOutboxError> {
        let job_id: String = selected.get(0)?;
        let selected_status: String = selected.get(11)?;
        let selected_token: Option<String> = selected.get(14)?;
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_outbox_ancestry_tx(&mut tx, &job_id).await?;
        let lock = if tx.driver() == DatabaseDriver::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let mut rows = tx.query(
            &format!("SELECT job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, thread_id, class, message_count, context_xml, status, attempt_count, policy_error_count, claim_token, summary_sender_jid, summary_body, approved_payload_xml, approved_publish_options_xml, queue_acceptance_may_exist, claimed_at_ms, next_attempt_at_ms FROM notification_outbox WHERE job_id = ?{lock}"),
            crate::db_params![job_id.as_str()],
        ).await?;
        let Some(current) = rows.next().await? else {
            tx.commit().await?;
            return Ok(());
        };
        drop(rows);
        let status: String = current.get(11)?;
        let token: Option<String> = current.get(14)?;
        let still_due = match status.as_str() {
            STATUS_QUEUED => current
                .get::<Option<i64>>(21)?
                .is_none_or(|next| next <= now_ms),
            STATUS_IN_PROGRESS => current
                .get::<Option<i64>>(20)?
                .is_some_and(|claimed| claimed <= stale_claimed_before_ms),
            _ => false,
        };
        if status != selected_status || token != selected_token || !still_due {
            tx.commit().await?;
            return Ok(());
        }
        let error = match decode_outbox_job(&current) {
            Ok(_) => {
                tx.commit().await?;
                return Ok(());
            }
            Err(error) => error,
        };
        tracing::warn!(%error, "malformed notification outbox job");
        let uncertain = current.get::<i64>(19)? != 0;
        // Any parent discovered after acquiring the child lock is taken NOWAIT.
        // A conflicting late attachment aborts this disposition without settling.
        if let Ok(id) = uuid::Uuid::parse_str(&job_id) {
            EffectDescendantRepository::lock_all_nowait_raw(&mut tx, id).await?;
        }
        let changed = tx.execute(
            "UPDATE notification_outbox SET status = ?, policy_error_count = 0, last_error = ?, next_attempt_at_ms = NULL, claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ? WHERE job_id = ? AND status = ? AND (claim_token = ? OR (claim_token IS NULL AND ? = 0))",
            crate::db_params![STATUS_FAILED, PublishFailureClass::Malformed.reason().as_str(), now_ms, job_id.as_str(), status.as_str(), token.as_deref(), i64::from(token.is_some())],
        ).await?;
        // Failed + uncertain is a parked quarantine, not provider refusal.
        // Its frozen bytes and unresolved custody survive until verified disposition.
        if changed > 0 && !uncertain {
            settle_outbox_if_unowned_tx(&mut tx, &job_id).await?;
        }
        tx.commit().await?;
        if changed > 0 {
            record_failure_transition(
                &job_id,
                "malformed",
                &PublishFailureClass::Malformed.with_error(&error),
                true,
                uncertain,
            );
        }
        Ok(())
    }

    pub(super) async fn persist_parked_outbox_probe(
        &self,
        batch_size: usize,
    ) -> Result<Vec<NotificationOutboxJob>, NotificationOutboxError> {
        let mut tx = self.db.begin_immediate().await?;
        let lock = if tx.driver() == DatabaseDriver::Postgres {
            " FOR UPDATE SKIP LOCKED"
        } else {
            ""
        };
        let mut rows = tx.query(
            &format!("SELECT job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, thread_id, class, message_count, context_xml, status, attempt_count, policy_error_count, claim_token, summary_sender_jid, summary_body, approved_payload_xml, approved_publish_options_xml, queue_acceptance_may_exist, next_attempt_at_ms FROM notification_outbox WHERE status = ? AND queue_acceptance_may_exist = 1 AND approved_payload_xml IS NOT NULL ORDER BY COALESCE(next_attempt_at_ms, 0), job_id LIMIT ?{lock}"),
            crate::db_params![STATUS_FAILED, batch_size.clamp(1, 1_000)],
        ).await?;
        let mut selected = Vec::new();
        while let Some(row) = rows.next().await? {
            selected.push(row);
        }
        drop(rows);
        let now = crate::time::now_ms();
        let mut jobs = Vec::new();
        for row in selected {
            let job_id: String = row.get(0)?;
            let previous_probe: Option<i64> = row.get(20)?;
            let next_probe = now.max(previous_probe.unwrap_or(0).saturating_add(1));
            tx.execute("UPDATE notification_outbox SET next_attempt_at_ms = ? WHERE job_id = ? AND status = ? AND queue_acceptance_may_exist = 1", crate::db_params![next_probe, job_id.as_str(), STATUS_FAILED]).await?;
            match decode_outbox_job(&row) {
                Ok(job) => jobs.push(job),
                Err(error) => {
                    tracing::warn!(job_id, %error, "parked notification proof probe skipped malformed row")
                }
            }
        }
        tx.commit().await?;
        Ok(jobs)
    }

    pub(super) async fn persist_terminal_notification_proof(
        &self,
        job: &NotificationOutboxJob,
        proof: CanonicalNotificationTerminalProof,
    ) -> Result<u64, NotificationOutboxError> {
        let Some(payload) = job.approved_payload.as_ref() else {
            return Ok(0);
        };
        let cancelled = proof == CanonicalNotificationTerminalProof::RegistrationRevoked;
        let status = if cancelled {
            STATUS_FAILED
        } else {
            STATUS_PUBLISHED
        };
        let now = crate::time::now_ms();
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
        let changed = tx.execute(
            "UPDATE notification_outbox SET status = ?, queue_acceptance_may_exist = CASE WHEN ? = 1 THEN 0 ELSE queue_acceptance_may_exist END, policy_error_count = 0, last_error = ?, next_attempt_at_ms = NULL, claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ?, published_at_ms = CASE WHEN ? = 1 THEN ? ELSE published_at_ms END WHERE job_id = ? AND status = ? AND (claim_token = ? OR (claim_token IS NULL AND ? = 1 AND queue_acceptance_may_exist = 1)) AND recipient_bare_jid = ? AND push_service_jid = ? AND node = ? AND approved_payload_xml = ? AND (approved_publish_options_xml = ? OR (approved_publish_options_xml IS NULL AND ? = 0))",
            crate::db_params![status, i64::from(cancelled), if cancelled {Some("canonical notification acceptance explicitly cancelled by registration revocation")} else {None}, now, i64::from(!cancelled), now, job.job_id.as_str(), if job.status == NotificationOutboxStatus::Failed { STATUS_FAILED } else { STATUS_IN_PROGRESS }, job.claim_token.as_deref(), i64::from(job.status == NotificationOutboxStatus::Failed), job.recipient_bare_jid.to_string(), job.push_service_jid.to_string(), job.node.as_str(), String::from(payload), job.approved_publish_options.as_ref().map(String::from), i64::from(job.approved_publish_options.is_some())],
        ).await?;
        if changed > 0 {
            settle_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
            crate::push_service::acknowledge_completed_outbox_tx(&mut tx, job.job_id.as_str())
                .await?;
        }
        tx.commit().await?;
        Ok(changed)
    }

    pub(super) async fn persist_gone_registration_failure(
        &self,
        job: &NotificationOutboxJob,
        clear_invocation_uncertainty: bool,
    ) -> Result<Option<bool>, NotificationOutboxError> {
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
        let lock = if tx.driver() == DatabaseDriver::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let mut rows = tx
            .query(
                &format!("SELECT queue_acceptance_may_exist FROM notification_outbox WHERE job_id = ? AND status = ? AND claim_token = ?{lock}"),
                crate::db_params![job.job_id.as_str(), STATUS_IN_PROGRESS, job.claim_token.as_deref()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let uncertain = row.get::<i64>(0)? != 0 && !clear_invocation_uncertainty;
        drop(rows);
        tx.execute(
            "UPDATE notification_outbox SET status = ?, queue_acceptance_may_exist = ?, policy_error_count = 0, last_error = ?, next_attempt_at_ms = NULL, claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ? WHERE job_id = ? AND status = ? AND claim_token = ?",
            crate::db_params![STATUS_FAILED, i64::from(uncertain), PublishFailureClass::RegistrationGone.reason().as_str(), crate::time::now_ms(), job.job_id.as_str(), STATUS_IN_PROGRESS, job.claim_token.as_deref()],
        ).await?;
        if !uncertain {
            settle_outbox_if_unowned_tx(&mut tx, job.job_id.as_str()).await?;
        }
        tx.commit().await?;
        Ok(Some(uncertain))
    }

    pub(super) async fn persist_job_failure_check(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<bool, NotificationOutboxError> {
        let mut rows = self
            .query(
                "SELECT status FROM notification_outbox WHERE job_id = ?",
                crate::db_params![job.job_id.as_str()],
            )
            .await?;
        let failed = match rows.next().await? {
            Some(row) => row.get::<String>(0)? == STATUS_FAILED,
            None => false,
        };
        Ok(failed)
    }

    pub(super) async fn persist_claimed_job_policy_retry(
        &self,
        job: &NotificationOutboxJob,
        failure: &PublishFailure,
        next_policy_error_count: i64,
        exhausted: bool,
    ) -> Result<PublisherTransition, NotificationOutboxError> {
        let status = if exhausted {
            STATUS_FAILED
        } else {
            STATUS_QUEUED
        };
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
        let row_lock = if tx.driver() == DatabaseDriver::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let mut rows = tx
            .query(
                &format!("SELECT queue_acceptance_may_exist FROM notification_outbox WHERE job_id = ?{row_lock}"),
                crate::db_params![job.job_id.as_str()],
            )
            .await?;
        let uncertain = match rows.next().await? {
            Some(row) => row.get::<i64>(0)? != 0,
            None => false,
        };
        drop(rows);
        let now_ms = crate::time::now_ms();
        let changed = tx.execute(
            "UPDATE notification_outbox SET status = ?, policy_error_count = ?, last_error = ?, next_attempt_at_ms = ?, claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ? WHERE job_id = ? AND status = ? AND claim_token = ?",
            crate::db_params![status, next_policy_error_count, failure.class.reason().as_str(), if exhausted { None } else { Some(now_ms.saturating_add(if uncertain { uncertain_retry_delay_ms(next_policy_error_count.saturating_add(job.attempt_count)) } else { policy_retry_delay_ms(next_policy_error_count) })) }, now_ms, job.job_id.as_str(), STATUS_IN_PROGRESS, job.claim_token.as_deref()],
        ).await?;
        if changed > 0 && exhausted && !uncertain {
            settle_outbox_if_unowned_tx(&mut tx, job.job_id.as_str()).await?;
        }
        tx.commit().await?;
        Ok(PublisherTransition { changed, uncertain })
    }

    pub(super) async fn persist_claimed_job_failure(
        &self,
        job: &NotificationOutboxJob,
        failure: &PublishFailure,
    ) -> Result<u64, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
        let affected = tx
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
              AND queue_acceptance_may_exist = 0
            "#,
                crate::db_params![
                    STATUS_FAILED,
                    failure.class.reason().as_str(),
                    now_ms,
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                ],
            )
            .await?;
        if affected > 0 {
            settle_outbox_if_unowned_tx(&mut tx, job.job_id.as_str()).await?;
        }
        tx.commit().await?;
        Ok(affected)
    }

    pub(super) async fn persist_claimed_job_retry(
        &self,
        job: &NotificationOutboxJob,
        failure: &PublishFailure,
        next_attempt_count: i64,
    ) -> Result<Option<PublisherRetryTransition>, NotificationOutboxError> {
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
        let row_lock = if tx.driver() == DatabaseDriver::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let mut rows = tx
            .query(
                &format!("SELECT queue_acceptance_may_exist FROM notification_outbox WHERE job_id = ?{row_lock}"),
                crate::db_params![job.job_id.as_str()],
            )
            .await?;
        let acceptance_may_exist = match rows.next().await? {
            Some(row) => row.get::<i64>(0)? != 0,
            None => false,
        };
        drop(rows);
        let now_ms = crate::time::now_ms();
        // Budget exhaustion stops scheduling even when acceptance remains unknown.
        // Failed + uncertain is a parked quarantine retaining payload and custody.
        let (status, next_attempt_at_ms) =
            if next_attempt_count.saturating_add(job.policy_error_count) >= MAX_OUTBOX_ATTEMPTS {
                (STATUS_FAILED, None)
            } else {
                (
                    STATUS_QUEUED,
                    Some(now_ms.saturating_add(if acceptance_may_exist {
                        uncertain_retry_delay_ms(
                            next_attempt_count.saturating_add(job.policy_error_count),
                        )
                    } else {
                        retry_delay_ms(next_attempt_count)
                    })),
                )
            };
        let affected = tx
            .execute(
                r#"
            UPDATE notification_outbox
            SET status = ?,
                attempt_count = ?,
                last_error = ?,
                next_attempt_at_ms = ?,
                claimed_at_ms = NULL,
                claim_token = NULL,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND (claim_token = ? OR (claim_token IS NULL AND ? = 0))
            "#,
                crate::db_params![
                    status,
                    next_attempt_count,
                    failure.class.reason().as_str(),
                    next_attempt_at_ms,
                    now_ms,
                    job.job_id.as_str(),
                    STATUS_IN_PROGRESS,
                    job.claim_token.as_deref(),
                    i64::from(job.claim_token.is_some()),
                ],
            )
            .await?;
        if affected == 0 {
            return Ok(None);
        }
        if status == STATUS_FAILED && !acceptance_may_exist {
            settle_outbox_if_unowned_tx(&mut tx, job.job_id.as_str()).await?;
        }
        tx.commit().await?;
        Ok(Some(PublisherRetryTransition {
            terminal: status == STATUS_FAILED,
            uncertain: acceptance_may_exist,
        }))
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

pub(super) async fn lock_outbox_ancestry_tx(
    tx: &mut Transaction<'_>,
    job_id: &str,
) -> Result<(), NotificationOutboxError> {
    lock_outbox_ancestry_with_api(tx, None, job_id).await
}

pub(super) async fn lock_outbox_ancestry_with_api(
    tx: &mut Transaction<'_>,
    api: Option<&DefinerApiCandidateContext<'_>>,
    job_id: &str,
) -> Result<(), NotificationOutboxError> {
    if let Ok(id) = uuid::Uuid::parse_str(job_id) {
        if let Some(api) = api {
            EffectDescendantRepository::lock_all_with_candidate_api(tx, api, id).await?;
        } else {
            EffectDescendantRepository::lock_all_nowait_raw(tx, id).await?;
        }
        let sql = if tx.driver() == DatabaseDriver::Postgres {
            "SELECT candidate_delivery_id FROM notification_outbox_lineage WHERE job_id = ? AND settled_at_ms IS NULL ORDER BY candidate_delivery_id FOR UPDATE"
        } else {
            "SELECT candidate_delivery_id FROM notification_outbox_lineage WHERE job_id = ? AND settled_at_ms IS NULL ORDER BY candidate_delivery_id"
        };
        let mut rows = tx.query(sql, crate::db_params![job_id]).await?;
        while rows.next().await?.is_some() {}
        if let Some(api) = api {
            EffectDescendantRepository::lock_all_with_candidate_api(tx, api, id).await?;
        } else {
            EffectDescendantRepository::lock_all_nowait_raw(tx, id).await?;
        }
    }
    Ok(())
}

pub(super) async fn settle_outbox_ancestry_tx(
    tx: &mut Transaction<'_>,
    job_id: &str,
) -> Result<(), NotificationOutboxError> {
    settle_outbox_ancestry_with_api(tx, None, job_id).await
}

pub(super) async fn settle_outbox_ancestry_with_api(
    tx: &mut Transaction<'_>,
    api: Option<&DefinerApiCandidateContext<'_>>,
    job_id: &str,
) -> Result<(), NotificationOutboxError> {
    if let Ok(id) = uuid::Uuid::parse_str(job_id) {
        if let Some(api) = api {
            EffectDescendantRepository::settle_with_candidate_api(tx, api, id, chrono::Utc::now())
                .await?;
        } else {
            EffectDescendantRepository::settle_all_raw(tx, id, chrono::Utc::now()).await?;
        }
        tx.execute("UPDATE notification_outbox_lineage SET settled_at_ms = ? WHERE job_id = ? AND settled_at_ms IS NULL",
            crate::db_params![crate::time::now_ms(), job_id]).await?;
    }
    Ok(())
}

/// Explicit owner-scoped registration retirement is authoritative even when a
/// crash left an uncertain outbox boundary without any provider queue row.
/// Call after cancelling provider-owned work in the same transaction.
pub(crate) async fn retire_registration_outbox_tx(
    tx: &mut Transaction<'_>,
    owner: &BareJid,
    service: Option<&BareJid>,
    node: Option<&str>,
) -> Result<u64, NotificationOutboxError> {
    retire_registration_outbox_with_api(tx, None, owner, service, node).await
}

pub(crate) async fn retire_registration_outbox_with_api(
    tx: &mut Transaction<'_>,
    api: Option<&DefinerApiCandidateContext<'_>>,
    owner: &BareJid,
    service: Option<&BareJid>,
    node: Option<&str>,
) -> Result<u64, NotificationOutboxError> {
    let exists = if tx.driver() == DatabaseDriver::Postgres {
        "SELECT 1 WHERE to_regclass('notification_outbox') IS NOT NULL"
    } else {
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'notification_outbox'"
    };
    let mut rows = tx.query(exists, ()).await?;
    let initialized = rows.next().await?.is_some();
    drop(rows);
    if !initialized {
        return Ok(0);
    }
    let foundation_exists = if tx.driver() == DatabaseDriver::Postgres {
        "SELECT 1 WHERE to_regclass('ingress_effect_descendants') IS NOT NULL"
    } else {
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ingress_effect_descendants'"
    };
    let mut rows = tx.query(foundation_exists, ()).await?;
    let has_foundation = rows.next().await?.is_some();
    drop(rows);
    let canonical_custody = if has_foundation {
        if let Some(api) = api {
            format!(
                "{}(notification_outbox.job_id, TRUE)",
                api.qualified_function("waddle_ingress_descendant_exists")
            )
        } else {
            "EXISTS (SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = notification_outbox.job_id AND settled_at IS NULL)".to_owned()
        }
    } else {
        "0 = 1".to_owned()
    };
    // Settled known failures are terminal history, including their original
    // diagnosis and retention tail. Only outstanding custody permits retirement.
    let eligible = format!("(status IN ('queued', 'in-progress') OR (status = 'failed' AND (queue_acceptance_may_exist = 1 OR {canonical_custody} OR EXISTS (SELECT 1 FROM notification_outbox_lineage WHERE job_id = notification_outbox.job_id AND settled_at_ms IS NULL))))");
    let service_raw = service.map(ToString::to_string);
    let mut rows = tx.query(
        &format!("SELECT job_id FROM notification_outbox WHERE recipient_bare_jid = ? AND (push_service_jid = ? OR ? = 0) AND (node = ? OR ? = 0) AND {eligible} ORDER BY job_id"),
        crate::db_params![owner.to_string(), service_raw.as_deref(), i64::from(service.is_some()), node, i64::from(node.is_some())],
    ).await?;
    let mut jobs = Vec::new();
    while let Some(row) = rows.next().await? {
        jobs.push(row.get::<String>(0)?);
    }
    drop(rows);
    let mut retired = 0_u64;
    for job_id in jobs {
        // Registration/node locks are held by the caller, so never wait while
        // reversing the canonical-parent-before-scheduler lock order.
        lock_outbox_ancestry_with_api(tx, api, &job_id).await?;
        let changed = tx.execute(
            &format!("UPDATE notification_outbox SET status = ?, queue_acceptance_may_exist = 0, last_error = 'registration-revoked', next_attempt_at_ms = NULL, claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ? WHERE job_id = ? AND recipient_bare_jid = ? AND (push_service_jid = ? OR ? = 0) AND (node = ? OR ? = 0) AND {eligible}"),
            crate::db_params![STATUS_FAILED, crate::time::now_ms(), job_id.as_str(), owner.to_string(), service_raw.as_deref(), i64::from(service.is_some()), node, i64::from(node.is_some())],
        ).await?;
        if changed > 0 {
            retired += changed;
            settle_outbox_if_unowned_with_api(tx, api, &job_id).await?;
            crate::push_service::acknowledge_completed_outbox_tx(tx, &job_id).await?;
        }
    }
    Ok(retired)
}

/// An upstream refusal cannot release a descendant already owned by the
/// durable provider queue. The provider worker settles that remaining custody.
pub(super) async fn settle_outbox_if_unowned_tx(
    tx: &mut Transaction<'_>,
    job_id: &str,
) -> Result<(), NotificationOutboxError> {
    settle_outbox_if_unowned_with_api(tx, None, job_id).await
}

pub(super) async fn settle_outbox_if_unowned_with_api(
    tx: &mut Transaction<'_>,
    api: Option<&DefinerApiCandidateContext<'_>>,
    job_id: &str,
) -> Result<(), NotificationOutboxError> {
    let has_queue = if tx.driver() == DatabaseDriver::Postgres {
        let mut rows = tx
            .query("SELECT to_regclass('push_publish_jobs')::text", ())
            .await?;
        match rows.next().await? {
            Some(row) => row.get::<Option<String>>(0)?.is_some(),
            None => false,
        }
    } else {
        let mut rows = tx
            .query(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'push_publish_jobs'",
                (),
            )
            .await?;
        rows.next().await?.is_some()
    };
    if has_queue {
        let mut rows = tx.query("SELECT 1 FROM push_publish_jobs WHERE ancestry_job_id = ? AND (status IN ('queued', 'in-progress') OR (status = 'failed' AND uncertain_send = 1 AND terminal_disposition IS NULL)) LIMIT 1", crate::db_params![job_id]).await?;
        if rows.next().await?.is_some() {
            return Ok(());
        }
    }
    settle_outbox_ancestry_with_api(tx, api, job_id).await
}
