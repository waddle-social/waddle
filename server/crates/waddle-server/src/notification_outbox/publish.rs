//! Outbox claim/publish lifecycle: claiming due jobs, XEP-0357 publish
//! of claimed jobs, retry backoff, and terminal failure.

use super::*;

const MAX_OUTBOX_ATTEMPTS: i64 = 5;
const BASE_RETRY_DELAY_MS: i64 = 5_000;
const BASE_POLICY_RETRY_DELAY_MS: i64 = 60_000;
const MAX_RETRY_DELAY_MS: i64 = 300_000;
const BASE_UNCERTAIN_RETRY_DELAY_MS: i64 = 300_000;
const MAX_UNCERTAIN_RETRY_DELAY_MS: i64 = 3_600_000;
pub(super) const OUTBOX_CLAIM_TIMEOUT_MS: i64 = 300_000;

/// Closed classification propagated through every publish retry/failure path.
/// Diagnostic error values are logged at their source, never used as labels.
#[derive(Debug, Clone, Copy, Error)]
enum PublishFailureClass {
    #[error("notification storage failed")]
    Storage,
    #[error("blocking policy lookup failed")]
    PolicyLookup,
    #[error("registration lookup failed")]
    RegistrationLookup,
    #[error("inbox lookup failed")]
    InboxLookup,
    #[error("non-first-party notification target")]
    InvalidTarget,
    #[error("recipient blocked notification sender")]
    Blocked,
    #[error("registration is no longer active")]
    RegistrationGone,
    #[error("notification claim is no longer current")]
    ClaimLost,
    #[error("queue invocation was refused")]
    QueueRefused,
    #[error("queue invocation acceptance is unknown")]
    QueueUnknown,
    #[error("malformed notification job")]
    Malformed,
    #[error("unclassified notification failure")]
    Unknown,
}

impl PublishFailureClass {
    fn reason(self) -> waddle_xmpp::telemetry::attributes::PushRetryReason {
        use waddle_xmpp::telemetry::attributes::PushRetryReason as Reason;
        match self {
            Self::Storage => Reason::Storage,
            Self::PolicyLookup => Reason::PolicyLookup,
            Self::RegistrationLookup => Reason::RegistrationLookup,
            Self::InboxLookup => Reason::InboxLookup,
            Self::InvalidTarget => Reason::InvalidTarget,
            Self::Blocked => Reason::Blocked,
            Self::RegistrationGone => Reason::RegistrationGone,
            Self::ClaimLost => Reason::ClaimLost,
            Self::QueueRefused => Reason::QueueRefused,
            Self::QueueUnknown => Reason::QueueUnknown,
            Self::Malformed => Reason::Malformed,
            Self::Unknown => Reason::Unknown,
        }
    }

    fn with_error(self, error: impl std::fmt::Display) -> PublishFailure {
        PublishFailure {
            class: self,
            diagnostic: error.to_string(),
        }
    }
}

/// Human diagnostic text stays attached to a typed failure until the durable
/// transition log. Only its closed class enters storage and metrics.
#[derive(Debug, Error)]
#[error("{diagnostic}")]
struct PublishFailure {
    class: PublishFailureClass,
    diagnostic: String,
}

impl From<PublishFailureClass> for PublishFailure {
    fn from(class: PublishFailureClass) -> Self {
        class.with_error(class)
    }
}

impl PublishFailure {
    fn from_store_error(error: &NotificationOutboxError) -> Self {
        let class = match error {
            NotificationOutboxError::Inbox(_) => PublishFailureClass::InboxLookup,
            NotificationOutboxError::Blocking(_) => PublishFailureClass::PolicyLookup,
            NotificationOutboxError::Database(_) | NotificationOutboxError::Ancestry(_) => {
                PublishFailureClass::Storage
            }
            NotificationOutboxError::Push(_) => PublishFailureClass::QueueUnknown,
            _ => PublishFailureClass::Malformed,
        };
        class.with_error(error)
    }
}

fn record_failure_transition(
    job_id: &str,
    notification_class: &str,
    failure: &PublishFailure,
    terminal: bool,
    uncertain: bool,
) {
    use waddle_xmpp::telemetry::attributes::{PushDisposition, PushFailureClass};
    let reason = failure.class.reason();
    let class = match failure.class {
        PublishFailureClass::QueueUnknown => PushFailureClass::UnknownAfterSend,
        PublishFailureClass::ClaimLost if uncertain => PushFailureClass::UnknownAfterSend,
        PublishFailureClass::QueueRefused => PushFailureClass::ProvenRefusal,
        _ => PushFailureClass::PreSend,
    };
    let disposition = match (terminal, uncertain) {
        (true, true) => PushDisposition::Parked,
        (true, false) if matches!(failure.class, PublishFailureClass::RegistrationGone) => {
            PushDisposition::RegistrationRevoked
        }
        (true, false) => PushDisposition::DeadLetter,
        (false, _) => PushDisposition::Retry,
    };
    if disposition == PushDisposition::DeadLetter {
        waddle_xmpp::telemetry::reliability::increment_push_outbox_dead_lettered();
    } else if disposition == PushDisposition::Retry {
        waddle_xmpp::telemetry::reliability::increment_push_outbox_retry_scheduled(reason);
    }
    waddle_xmpp::telemetry::reliability::increment_push_disposition(reason, class, disposition);
    let push_stage = match disposition {
        PushDisposition::Retry => "retry_scheduled",
        PushDisposition::Parked => "parked",
        PushDisposition::RegistrationRevoked => "registration_gone",
        _ => "dead_lettered",
    };
    tracing::warn!(job_id, notification_class, push_stage, failure = reason.as_str(), failure_class = class.as_str(), custody_uncertain = uncertain, disposition = disposition.as_str(), error = %failure, "push pipeline transition");
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

        let mut claimed = Vec::new();
        for job in selected {
            if job.status == NotificationOutboxStatus::InProgress {
                // An expired claim is a failed attempt, not permission to
                // resend immediately. Persist the same uncertainty-aware
                // backoff and budget transition used by a live worker.
                self.schedule_retry_or_fail_with_failure(&job, PublishFailureClass::ClaimLost)
                    .await?;
                continue;
            }
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
                    let claimed_job = decode_outbox_job(&row)?;
                    if claimed_job
                        .attempt_count
                        .saturating_add(claimed_job.policy_error_count)
                        >= MAX_OUTBOX_ATTEMPTS
                    {
                        self.schedule_retry_or_fail_with_failure(
                            &claimed_job,
                            PublishFailureClass::ClaimLost,
                        )
                        .await?;
                    } else {
                        claimed.push(claimed_job);
                    }
                }
            }
        }
        Ok(claimed)
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
        let mut tx = self.db.begin_immediate().await?;
        if tx.driver() == crate::db::DatabaseDriver::Postgres {
            tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
                .await?;
        }
        lock_outbox_ancestry_tx(&mut tx, &job_id).await?;
        let lock = if tx.driver() == crate::db::DatabaseDriver::Postgres {
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
            crate::ingress_uow::EffectDescendantRepository::lock_all_nowait_raw(&mut tx, id)
                .await?;
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
                    tracing::warn!(job_id = job.job_id.as_str(), %error, "notification publish failed");
                    self.retry_or_fail_outcome_for_claimed_job(
                        &job,
                        PublishFailure::from_store_error(&error),
                    )
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
                    tracing::info!(
                        notification_class = job.class().as_db_value(),
                        push_stage = "published",
                        "push pipeline transition"
                    );
                }
                NotificationOutboxPublishOutcome::RetryScheduled { .. }
                | NotificationOutboxPublishOutcome::Failed { .. } => {}
                NotificationOutboxPublishOutcome::Suppressed { .. } => {
                    tracing::info!(
                        notification_class = job.class().as_db_value(),
                        push_stage = "suppressed",
                        suppression_reason = SuppressedReason::UnreadZeroAtPublish.as_db_value(),
                        "push pipeline transition"
                    );
                    waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                        SuppressedReason::UnreadZeroAtPublish.telemetry_reason(),
                    );
                }
            }
            outcomes.push(outcome);
        }
        // Terminal-only housekeeping never blocks ordinary due work. A parked
        // owner's storage/proof contention must remain isolated to that owner.
        match self
            .reconcile_parked_outbox_jobs(push_service, batch_size)
            .await
        {
            Ok(completed) => outcomes.extend(completed),
            Err(error) => tracing::warn!(%error, "parked notification reconciliation scan failed"),
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
        if job.approved_payload.is_some() && self.claimed_job_is_current(job).await? {
            let item = job.to_xep0357_pubsub_item();
            let delivery = uuid::Uuid::parse_str(job.job_id.as_str())
                .map_err(|_| NotificationOutboxError::InvalidDeliveryIdentity)?;
            let proof = match push_service
                .canonical_terminal_notification_proof(
                    &job.push_service_jid,
                    &job.node,
                    &item,
                    &job.recipient_bare_jid,
                    job.approved_publish_options.as_ref(),
                    delivery,
                )
                .await
            {
                Ok(proof) => proof,
                Err(error) => {
                    // This lookup precedes queue invocation. A storage error
                    // here is not evidence of unknown queue acceptance.
                    return self
                        .retry_or_fail_outcome_for_claimed_job(
                            job,
                            PublishFailureClass::Storage.with_error(error),
                        )
                        .await;
                }
            };
            if let Some(proof) = proof {
                if self
                    .complete_claimed_job_from_terminal_proof(job, proof)
                    .await?
                {
                    self.acknowledge_provider_owner_completion(
                        push_service,
                        job,
                        &item,
                        job.approved_publish_options.as_ref(),
                        delivery,
                    )
                    .await;
                    return Ok(match proof {
                        crate::push_service::CanonicalNotificationTerminalProof::ProviderCompleted => NotificationOutboxPublishOutcome::Published { job_id: job.job_id.clone(), item_id: job.job_id.as_str().to_string() },
                        crate::push_service::CanonicalNotificationTerminalProof::RegistrationRevoked => NotificationOutboxPublishOutcome::Failed { job_id: job.job_id.clone() },
                    });
                }
                return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                    job_id: job.job_id.clone(),
                });
            }
        }

        if job.push_service_jid() != first_party_service_jid {
            if self
                .mark_job_failed_with_failure(job, PublishFailureClass::InvalidTarget)
                .await?
            {
                return Ok(NotificationOutboxPublishOutcome::Failed {
                    job_id: job.job_id.clone(),
                });
            }
            return self
                .retry_or_fail_outcome_for_claimed_job(job, PublishFailureClass::InvalidTarget)
                .await;
        }

        match xep0191_blocks_notification_job(job, blocking_storage).await {
            Ok(true) => {
                if self
                    .mark_job_failed_with_failure(job, PublishFailureClass::Blocked)
                    .await?
                {
                    return Ok(NotificationOutboxPublishOutcome::Failed {
                        job_id: job.job_id.clone(),
                    });
                }
                return self
                    .retry_or_fail_outcome_for_claimed_job(job, PublishFailureClass::Blocked)
                    .await;
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(job_id = job.job_id.as_str(), failure = "policy_lookup", %error, "XEP-0191 blocklist load failed");
                return self
                    .defer_claimed_job_without_attempt(
                        job,
                        PublishFailureClass::PolicyLookup.with_error(&error),
                    )
                    .await;
            }
        }

        let registrations = push_store
            .get_for_user(&job.recipient_bare_jid.to_string())
            .await;
        let registrations = match registrations {
            Ok(registrations) => registrations,
            Err(error) => {
                tracing::warn!(job_id = job.job_id.as_str(), failure = "registration_lookup", %error, "notification registration lookup failed");
                return self
                    .retry_or_fail_outcome_for_claimed_job(
                        job,
                        PublishFailureClass::RegistrationLookup.with_error(&error),
                    )
                    .await;
            }
        };
        let service = job.push_service_jid.to_string();
        let registration = registrations.into_iter().find(|registration| {
            registration.service_jid == service
                && registration.node.as_deref() == Some(job.node.as_str())
        });
        let Some(registration) = registration else {
            return self
                .fail_claimed_job_for_gone_registration(job, false)
                .await;
        };

        if !self.claimed_job_is_current(job).await? {
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        }

        let unread = if job.queue_acceptance_may_exist {
            None
        } else {
            current_unread_count_for_job(job, inbox_storage).await?
        };
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
            return self
                .retry_or_fail_outcome_for_claimed_job(job, PublishFailureClass::ClaimLost)
                .await;
        }
        let message_count = unread.unwrap_or(0);
        // XEP-0357 `message-count` stays scoped to this conversation and
        // thread. APNs reads the account-wide total immediately before send
        // so a delayed retry cannot replay an old absolute app-icon badge.
        let item = job.to_xep0357_pubsub_item_with_count(message_count);
        let Some((item, publish_options)) = self
            .freeze_approved_item(job, &item, registration.publish_options.as_ref())
            .await?
        else {
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        };
        let Some(prior_acceptance_may_exist) = self.begin_queue_acceptance(job).await? else {
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        };
        match push_service
            .enqueue_registered_notification_from_user_server_with_publish_options(
                &job.push_service_jid,
                job.node.as_str(),
                &item,
                &job.recipient_bare_jid,
                publish_options.as_ref(),
                uuid::Uuid::parse_str(job.job_id.as_str())
                    .map_err(|_| NotificationOutboxError::InvalidDeliveryIdentity)?,
            )
            .await
        {
            Ok(result) => {
                if !self.mark_job_published(job).await? {
                    return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                        job_id: job.job_id.clone(),
                    });
                }
                self.acknowledge_provider_owner_completion(
                    push_service,
                    job,
                    &item,
                    publish_options.as_ref(),
                    uuid::Uuid::parse_str(job.job_id.as_str())
                        .map_err(|_| NotificationOutboxError::InvalidDeliveryIdentity)?,
                )
                .await;
                Ok(NotificationOutboxPublishOutcome::Published {
                    job_id: job.job_id.clone(),
                    item_id: result.item_id().to_string(),
                })
            }
            Err(crate::push_service::PushQueueAcceptanceError::KnownNotAccepted(error)) => {
                if matches!(
                    error,
                    waddle_xmpp::XmppError::Stanza {
                        condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
                        ..
                    }
                ) {
                    tracing::warn!(job_id = job.job_id.as_str(), failure = "registration_gone", %error, "notification registration no longer accepts publications");
                    return self
                        .fail_claimed_job_for_gone_registration(job, !prior_acceptance_may_exist)
                        .await;
                }
                self.record_known_queue_refusal(job, prior_acceptance_may_exist)
                    .await?;
                tracing::warn!(job_id = job.job_id.as_str(), failure = "queue_refused", %error, "notification queue refused invocation");
                self.retry_or_fail_outcome_for_claimed_job(
                    job,
                    PublishFailureClass::QueueRefused.with_error(&error),
                )
                .await
            }
            Err(crate::push_service::PushQueueAcceptanceError::Unknown(error)) => {
                tracing::warn!(job_id = job.job_id.as_str(), failure = "queue_unknown", %error, "notification queue acceptance unknown");
                self.retry_or_fail_outcome_for_claimed_job(
                    job,
                    PublishFailureClass::QueueUnknown.with_error(&error),
                )
                .await
            }
        }
    }

    /// Parking stops sends, but exact provider completion/revocation proof can
    /// still close upstream custody. This path never invokes queue acceptance.
    async fn reconcile_parked_outbox_jobs(
        &self,
        push_service: &crate::push_service::DatabasePushServiceStore,
        batch_size: usize,
    ) -> Result<Vec<NotificationOutboxPublishOutcome>, NotificationOutboxError> {
        // Failed jobs never re-enter the send scheduler. Reuse their otherwise
        // idle retry timestamp as a durable probe cursor: oldest probes rotate
        // to the back even when no proof is available or completion contends.
        // Row locks are released before provider lookups and parent NOWAIT locks.
        let mut tx = self.db.begin_immediate().await?;
        let lock = if tx.driver() == crate::db::DatabaseDriver::Postgres {
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
        let mut completed = Vec::new();
        for job in jobs {
            let item = job.to_xep0357_pubsub_item();
            let delivery = match uuid::Uuid::parse_str(job.job_id.as_str()) {
                Ok(delivery) => delivery,
                Err(error) => {
                    tracing::warn!(job_id = job.job_id.as_str(), %error, "parked notification proof probe has invalid identity");
                    continue;
                }
            };
            let proof = match push_service
                .canonical_terminal_notification_proof(
                    &job.push_service_jid,
                    &job.node,
                    &item,
                    &job.recipient_bare_jid,
                    job.approved_publish_options.as_ref(),
                    delivery,
                )
                .await
            {
                Ok(Some(proof)) => proof,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(job_id = job.job_id.as_str(), %error, "parked notification terminal proof lookup failed");
                    continue;
                }
            };
            let completed_owner = match self
                .complete_claimed_job_from_terminal_proof(&job, proof)
                .await
            {
                Ok(completed) => completed,
                Err(error) => {
                    tracing::warn!(job_id = job.job_id.as_str(), %error, "parked notification terminal completion deferred");
                    continue;
                }
            };
            if completed_owner {
                if proof
                    == crate::push_service::CanonicalNotificationTerminalProof::ProviderCompleted
                {
                    waddle_xmpp::telemetry::reliability::increment_push_outbox_published();
                    tracing::info!(
                        job_id = job.job_id.as_str(),
                        notification_class = job.class.as_db_value(),
                        push_stage = "published",
                        "parked push terminal proof reconciled"
                    );
                }
                self.acknowledge_provider_owner_completion(
                    push_service,
                    &job,
                    &item,
                    job.approved_publish_options.as_ref(),
                    delivery,
                )
                .await;
                completed.push(match proof {
                    crate::push_service::CanonicalNotificationTerminalProof::ProviderCompleted => NotificationOutboxPublishOutcome::Published { job_id: job.job_id.clone(), item_id: job.job_id.as_str().to_string() },
                    crate::push_service::CanonicalNotificationTerminalProof::RegistrationRevoked => NotificationOutboxPublishOutcome::Failed { job_id: job.job_id.clone() },
                });
            }
        }
        Ok(completed)
    }

    async fn complete_claimed_job_from_terminal_proof(
        &self,
        job: &NotificationOutboxJob,
        proof: crate::push_service::CanonicalNotificationTerminalProof,
    ) -> Result<bool, NotificationOutboxError> {
        let Some(payload) = job.approved_payload.as_ref() else {
            return Ok(false);
        };
        let cancelled =
            proof == crate::push_service::CanonicalNotificationTerminalProof::RegistrationRevoked;
        let status = if cancelled {
            STATUS_FAILED
        } else {
            STATUS_PUBLISHED
        };
        let now = crate::time::now_ms();
        let mut tx = self.db.begin_immediate().await?;
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
        if changed > 0 && cancelled {
            waddle_xmpp::telemetry::reliability::increment_push_registration_revoked();
            tracing::info!(
                job_id = job.job_id.as_str(),
                notification_class = job.class.as_db_value(),
                push_stage = "registration_revoked",
                disposition = "registration_revoked",
                "push pipeline transition"
            );
        }
        Ok(changed > 0)
    }

    /// Completion is already committed. A separate-store ACK failure cannot
    /// reopen its owner; unresolved provider proof remains conservatively retained.
    async fn acknowledge_provider_owner_completion(
        &self,
        push_service: &crate::push_service::DatabasePushServiceStore,
        job: &NotificationOutboxJob,
        item: &PubSubItem,
        options: Option<&Element>,
        delivery: uuid::Uuid,
    ) {
        match tokio::time::timeout(
            std::time::Duration::from_secs(1),
            push_service.acknowledge_canonical_outbox_completion(
                &job.push_service_jid,
                &job.node,
                item,
                &job.recipient_bare_jid,
                options,
                delivery,
            ),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, "upstream completion committed but provider proof acknowledgment failed; proof retained");
            }
            Err(_) => {
                tracing::warn!("upstream completion committed but provider proof acknowledgment timed out; proof retained");
            }
        }
    }

    /// Persist uncertainty before the queue call, including cancellation between
    /// this commit and invocation. A stale claim cannot enter that boundary.
    async fn begin_queue_acceptance(
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

    /// A definite refusal clears only the uncertainty created by this invocation.
    /// Prior unknown/accepted work, or a successor claim, retains custody.
    async fn record_known_queue_refusal(
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

    /// A gone target cannot recover through retries of this registration. Stop
    /// this exact claim without discharging any earlier uncertain acceptance.
    /// The Push Service retires its configured registration under the node's
    /// owner lock during admission; a delayed refusal must not disable a later
    /// re-enable or acquire authority over a separate registration store.
    async fn fail_claimed_job_for_gone_registration(
        &self,
        job: &NotificationOutboxJob,
        clear_invocation_uncertainty: bool,
    ) -> Result<NotificationOutboxPublishOutcome, NotificationOutboxError> {
        let mut tx = self.db.begin_immediate().await?;
        lock_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
        let lock = if tx.driver() == crate::db::DatabaseDriver::Postgres {
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
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
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
        record_failure_transition(
            job.job_id.as_str(),
            job.class.as_db_value(),
            &PublishFailureClass::RegistrationGone.into(),
            true,
            uncertain,
        );
        Ok(NotificationOutboxPublishOutcome::Failed {
            job_id: job.job_id.clone(),
        })
    }

    /// Freeze the approved notification before crossing the durable acceptance
    /// boundary. A lost reply must replay the same key and payload/options.
    async fn freeze_approved_item(
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
        failure: impl Into<PublishFailure>,
    ) -> Result<NotificationOutboxPublishOutcome, NotificationOutboxError> {
        let failure = failure.into();
        let Some(_) = self
            .schedule_retry_or_fail_with_failure(job, failure)
            .await?
        else {
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        };
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
        if failed {
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
        failure: impl Into<PublishFailure>,
    ) -> Result<NotificationOutboxPublishOutcome, NotificationOutboxError> {
        let failure = failure.into();
        let next_policy_error_count = job.policy_error_count.saturating_add(1);
        let exhausted =
            next_policy_error_count.saturating_add(job.attempt_count) >= MAX_OUTBOX_ATTEMPTS;
        let status = if exhausted {
            STATUS_FAILED
        } else {
            STATUS_QUEUED
        };
        let mut tx = self.db.begin_immediate().await?;
        lock_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
        let row_lock = if tx.driver() == crate::db::DatabaseDriver::Postgres {
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
        if changed > 0 {
            record_failure_transition(
                job.job_id.as_str(),
                job.class.as_db_value(),
                &failure,
                exhausted,
                uncertain,
            );
        }
        if changed > 0 && exhausted {
            Ok(NotificationOutboxPublishOutcome::Failed {
                job_id: job.job_id.clone(),
            })
        } else {
            Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            })
        }
    }

    pub async fn mark_job_published(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<bool, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let mut tx = self.db.begin_immediate().await?;
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
        let mut tx = self.db.begin_immediate().await?;
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

    #[cfg(test)]
    async fn mark_job_failed(
        &self,
        job: &NotificationOutboxJob,
        error: &str,
    ) -> Result<bool, NotificationOutboxError> {
        tracing::warn!(error, "notification job failed");
        self.mark_job_failed_with_failure(job, PublishFailureClass::Unknown)
            .await
    }

    async fn mark_job_failed_with_failure(
        &self,
        job: &NotificationOutboxJob,
        failure: impl Into<PublishFailure>,
    ) -> Result<bool, NotificationOutboxError> {
        let failure = failure.into();
        let now_ms = crate::time::now_ms();
        let mut tx = self.db.begin_immediate().await?;
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
        if affected > 0 {
            record_failure_transition(
                job.job_id.as_str(),
                job.class.as_db_value(),
                &failure,
                true,
                false,
            );
        }
        Ok(affected > 0)
    }

    pub async fn schedule_retry_or_fail(
        &self,
        job: &NotificationOutboxJob,
        error: String,
    ) -> Result<Option<i64>, NotificationOutboxError> {
        tracing::warn!(%error, "notification retry requested");
        self.schedule_retry_or_fail_with_failure(
            job,
            PublishFailureClass::Unknown.with_error(error),
        )
        .await
    }

    async fn schedule_retry_or_fail_with_failure(
        &self,
        job: &NotificationOutboxJob,
        failure: impl Into<PublishFailure>,
    ) -> Result<Option<i64>, NotificationOutboxError> {
        let failure = failure.into();
        let next_attempt_count = job.attempt_count.saturating_add(1);
        let mut tx = self.db.begin_immediate().await?;
        lock_outbox_ancestry_tx(&mut tx, job.job_id.as_str()).await?;
        let row_lock = if tx.driver() == crate::db::DatabaseDriver::Postgres {
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
        record_failure_transition(
            job.job_id.as_str(),
            job.class.as_db_value(),
            &failure,
            status == STATUS_FAILED,
            acceptance_may_exist,
        );
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

/// Unknown queue acceptance receives a longer recovery window before parking:
/// the four retry waits have a nominal total of 75 minutes. Jitter is bounded
/// after applying it, so an individual uncertain wait never exceeds one hour.
fn uncertain_retry_delay_ms(combined_failure_count: i64) -> i64 {
    let exponent = combined_failure_count.saturating_sub(1).clamp(0, 10) as u32;
    apply_retry_jitter(BASE_UNCERTAIN_RETRY_DELAY_MS.saturating_mul(2_i64.saturating_pow(exponent)))
        .min(MAX_UNCERTAIN_RETRY_DELAY_MS)
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

pub(super) async fn lock_outbox_ancestry_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
) -> Result<(), NotificationOutboxError> {
    if let Ok(id) = uuid::Uuid::parse_str(job_id) {
        crate::ingress_uow::EffectDescendantRepository::lock_all_nowait_raw(tx, id).await?;
        let sql = if tx.driver() == crate::db::DatabaseDriver::Postgres {
            "SELECT candidate_delivery_id FROM notification_outbox_lineage WHERE job_id = ? AND settled_at_ms IS NULL ORDER BY candidate_delivery_id FOR UPDATE"
        } else {
            "SELECT candidate_delivery_id FROM notification_outbox_lineage WHERE job_id = ? AND settled_at_ms IS NULL ORDER BY candidate_delivery_id"
        };
        let mut rows = tx.query(sql, crate::db_params![job_id]).await?;
        while rows.next().await?.is_some() {}
        crate::ingress_uow::EffectDescendantRepository::lock_all_nowait_raw(tx, id).await?;
    }
    Ok(())
}

pub(super) async fn settle_outbox_ancestry_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
) -> Result<(), NotificationOutboxError> {
    if let Ok(id) = uuid::Uuid::parse_str(job_id) {
        crate::ingress_uow::EffectDescendantRepository::settle_all_raw(tx, id, chrono::Utc::now())
            .await?;
        tx.execute("UPDATE notification_outbox_lineage SET settled_at_ms = ? WHERE job_id = ? AND settled_at_ms IS NULL",
            crate::db_params![crate::time::now_ms(), job_id]).await?;
    }
    Ok(())
}

/// Explicit owner-scoped registration retirement is authoritative even when a
/// crash left an uncertain outbox boundary without any provider queue row.
/// Call after cancelling provider-owned work in the same transaction.
pub(crate) async fn retire_registration_outbox_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner: &BareJid,
    service: Option<&BareJid>,
    node: Option<&str>,
) -> Result<u64, NotificationOutboxError> {
    let exists = if tx.driver() == crate::db::DatabaseDriver::Postgres {
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
    let foundation_exists = if tx.driver() == crate::db::DatabaseDriver::Postgres {
        "SELECT 1 WHERE to_regclass('ingress_effect_descendants') IS NOT NULL"
    } else {
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ingress_effect_descendants'"
    };
    let mut rows = tx.query(foundation_exists, ()).await?;
    let has_foundation = rows.next().await?.is_some();
    drop(rows);
    let canonical_custody = if has_foundation {
        "EXISTS (SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = notification_outbox.job_id AND settled_at IS NULL)"
    } else {
        "0 = 1"
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
        lock_outbox_ancestry_tx(tx, &job_id).await?;
        let changed = tx.execute(
            &format!("UPDATE notification_outbox SET status = ?, queue_acceptance_may_exist = 0, last_error = 'registration-revoked', next_attempt_at_ms = NULL, claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ? WHERE job_id = ? AND recipient_bare_jid = ? AND (push_service_jid = ? OR ? = 0) AND (node = ? OR ? = 0) AND {eligible}"),
            crate::db_params![STATUS_FAILED, crate::time::now_ms(), job_id.as_str(), owner.to_string(), service_raw.as_deref(), i64::from(service.is_some()), node, i64::from(node.is_some())],
        ).await?;
        if changed > 0 {
            retired += changed;
            settle_outbox_if_unowned_tx(tx, &job_id).await?;
            crate::push_service::acknowledge_completed_outbox_tx(tx, &job_id).await?;
        }
    }
    Ok(retired)
}

/// An upstream refusal cannot release a descendant already owned by the
/// durable provider queue. The provider worker settles that remaining custody.
pub(super) async fn settle_outbox_if_unowned_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
) -> Result<(), NotificationOutboxError> {
    let has_queue = if tx.driver() == crate::db::DatabaseDriver::Postgres {
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
    settle_outbox_ancestry_tx(tx, job_id).await
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
    async fn xep0357_summary_count_is_scoped_to_the_current_conversation() {
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

        let context = payload
            .children()
            .find(|child| child.is("context", super::super::payload::WADDLE_PUSH_CONTEXT_NS))
            .expect("Waddle context");
        assert_eq!(context.attr("badge-count"), None);
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

#[cfg(test)]
#[path = "acceptance_fence_tests.rs"]
mod acceptance_fence_tests;

#[cfg(test)]
#[path = "malformed_outbox_tests.rs"]
mod malformed_outbox_tests;

#[cfg(test)]
#[path = "retry_policy_tests.rs"]
mod retry_policy_tests;

#[cfg(test)]
#[path = "gone_registration_tests.rs"]
mod gone_registration_tests;
