//! Outbox claim/publish lifecycle: claiming due jobs, XEP-0357 publish
//! of claimed jobs, retry backoff, and terminal failure.

#[cfg(test)]
use super::publisher_persistence::retire_registration_outbox_tx;
use super::publisher_persistence::{current_unread_count_for_job, PublisherTransition};
use super::{
    xep0191_blocks_notification_job, BareJid, BlockingStorage, Element, Error, InboxStorage,
    NotificationOutboxError, NotificationOutboxJob, NotificationOutboxPublishOutcome,
    NotificationOutboxStatus, NotificationOutboxStore, PubSubItem, PushSubscriptionStore,
    SuppressedReason,
};
#[cfg(test)]
use super::{STATUS_FAILED, STATUS_IN_PROGRESS, STATUS_PUBLISHED, STATUS_QUEUED};
use crate::push_service::CanonicalNotificationTerminalProof;
use crate::push_service::DatabasePushServiceStore;

pub(super) const MAX_OUTBOX_ATTEMPTS: i64 = 5;
const BASE_RETRY_DELAY_MS: i64 = 5_000;
const BASE_POLICY_RETRY_DELAY_MS: i64 = 60_000;
const MAX_RETRY_DELAY_MS: i64 = 300_000;
const BASE_UNCERTAIN_RETRY_DELAY_MS: i64 = 300_000;
const MAX_UNCERTAIN_RETRY_DELAY_MS: i64 = 3_600_000;
pub(super) const OUTBOX_CLAIM_TIMEOUT_MS: i64 = 300_000;

/// Closed classification propagated through every publish retry/failure path.
/// Diagnostic error values are logged at their source, never used as labels.
#[derive(Debug, Clone, Copy, Error)]
pub(super) enum PublishFailureClass {
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
    pub(super) fn reason(self) -> waddle_xmpp::telemetry::attributes::PushRetryReason {
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

    pub(super) fn with_error(self, error: impl std::fmt::Display) -> PublishFailure {
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
pub(super) struct PublishFailure {
    pub(super) class: PublishFailureClass,
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

pub(super) fn record_failure_transition(
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
    pub async fn claim_due_outbox_jobs(
        &self,
        batch_size: usize,
    ) -> Result<Vec<NotificationOutboxJob>, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 1_000);
        let now_ms = crate::time::now_ms();
        let stale_claimed_before_ms = now_ms.saturating_sub(OUTBOX_CLAIM_TIMEOUT_MS);
        let selected = self
            .persist_due_outbox_selection(batch_size, now_ms, stale_claimed_before_ms)
            .await?;

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
            if let Some(claimed_job) = self.persist_outbox_claim(&job, now_ms, claim_token).await? {
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
        Ok(claimed)
    }

    pub async fn drain_due_outbox_jobs(
        &self,
        push_service: &DatabasePushServiceStore,
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
        push_service: &DatabasePushServiceStore,
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
                        CanonicalNotificationTerminalProof::ProviderCompleted => {
                            NotificationOutboxPublishOutcome::Published {
                                job_id: job.job_id.clone(),
                                item_id: job.job_id.as_str().to_string(),
                            }
                        }
                        CanonicalNotificationTerminalProof::RegistrationRevoked => {
                            NotificationOutboxPublishOutcome::Failed {
                                job_id: job.job_id.clone(),
                            }
                        }
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
        push_service: &DatabasePushServiceStore,
        batch_size: usize,
    ) -> Result<Vec<NotificationOutboxPublishOutcome>, NotificationOutboxError> {
        // Failed jobs never re-enter the send scheduler. Reuse their otherwise
        // idle retry timestamp as a durable probe cursor: oldest probes rotate
        // to the back even when no proof is available or completion contends.
        // Row locks are released before provider lookups and parent NOWAIT locks.
        let jobs = self.persist_parked_outbox_probe(batch_size).await?;
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
                if proof == CanonicalNotificationTerminalProof::ProviderCompleted {
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
                    CanonicalNotificationTerminalProof::ProviderCompleted => {
                        NotificationOutboxPublishOutcome::Published {
                            job_id: job.job_id.clone(),
                            item_id: job.job_id.as_str().to_string(),
                        }
                    }
                    CanonicalNotificationTerminalProof::RegistrationRevoked => {
                        NotificationOutboxPublishOutcome::Failed {
                            job_id: job.job_id.clone(),
                        }
                    }
                });
            }
        }
        Ok(completed)
    }

    async fn complete_claimed_job_from_terminal_proof(
        &self,
        job: &NotificationOutboxJob,
        proof: CanonicalNotificationTerminalProof,
    ) -> Result<bool, NotificationOutboxError> {
        let cancelled = proof == CanonicalNotificationTerminalProof::RegistrationRevoked;
        let changed = self.persist_terminal_notification_proof(job, proof).await?;
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
        push_service: &DatabasePushServiceStore,
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
        self.persist_queue_acceptance(job).await
    }

    /// A definite refusal clears only the uncertainty created by this invocation.
    /// Prior unknown/accepted work, or a successor claim, retains custody.
    async fn record_known_queue_refusal(
        &self,
        job: &NotificationOutboxJob,
        prior_acceptance_may_exist: bool,
    ) -> Result<(), NotificationOutboxError> {
        self.persist_known_queue_refusal(job, prior_acceptance_may_exist)
            .await
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
        let Some(uncertain) = self
            .persist_gone_registration_failure(job, clear_invocation_uncertainty)
            .await?
        else {
            return Ok(NotificationOutboxPublishOutcome::RetryScheduled {
                job_id: job.job_id.clone(),
            });
        };
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
        self.persist_approved_item(job, item, publish_options).await
    }

    async fn claimed_job_is_current(
        &self,
        job: &NotificationOutboxJob,
    ) -> Result<bool, NotificationOutboxError> {
        self.persist_claim_currency_check(job).await
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
        let failed = self.persist_job_failure_check(job).await?;
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
        let PublisherTransition { changed, uncertain } = self
            .persist_claimed_job_policy_retry(job, &failure, next_policy_error_count, exhausted)
            .await?;
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
        self.persist_claimed_job_suppression(job).await
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
        let affected = self.persist_claimed_job_failure(job, &failure).await?;
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
        let Some(transition) = self
            .persist_claimed_job_retry(job, &failure, next_attempt_count)
            .await?
        else {
            return Ok(None);
        };
        record_failure_transition(
            job.job_id.as_str(),
            job.class.as_db_value(),
            &failure,
            transition.terminal,
            transition.uncertain,
        );
        Ok(Some(next_attempt_count))
    }
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
pub(super) fn uncertain_retry_delay_ms(combined_failure_count: i64) -> i64 {
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

#[cfg(test)]
mod tests {
    use waddle_xmpp::xep::NS_DATA_FORMS;

    use super::*;
    use crate::notification_outbox::test_support::{
        bare, candidate, enqueue_jobs_for_test, inbox_with_unread, reclaim_stale_job, store, target,
    };

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
