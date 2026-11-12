//! Fixed persistence operations for the T1 notification candidate drain.
//!
//! Each operation borrows the outbox store's existing database handle and
//! retains the original transaction boundary and admission lane.

use super::{
    decode_candidate, decode_sender_jids, encode_sender_jids, lock_outbox_ancestry_tx,
    policy_retry_delay_ms, require_full_sender_jid, require_full_sender_jid_set,
    require_sender_matches_conversation, require_sender_set_contains_scalar,
    require_sender_set_matches_conversation, settle_outbox_if_unowned_tx, Element,
    NotificationCandidate, NotificationOutboxError, NotificationOutboxJobId,
    NotificationOutboxStore, NotificationOutboxTarget, RichSummary, Row, SuppressedReason,
    STATUS_FAILED, STATUS_QUEUED,
};
use crate::db::Transaction;
use crate::ingress_substrate::EnteredTransaction;
use crate::ingress_uow::EffectDescendantRepository;

impl NotificationOutboxStore {
    pub(super) async fn persist_suppressed_candidate(
        &self,
        candidate: &NotificationCandidate,
        reason: SuppressedReason,
        now_ms: i64,
    ) -> Result<bool, NotificationOutboxError> {
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_candidate_ancestry_tx(&mut tx, candidate).await?;
        record_candidate_suppressed_reason_tx(&mut tx, candidate, reason).await?;
        let claimed = mark_candidate_outboxed_tx(&mut tx, candidate, now_ms).await?;
        if claimed > 0 {
            settle_candidate_ancestry_tx(&mut tx, candidate).await?;
        }
        tx.commit().await?;
        Ok(claimed > 0)
    }

    pub(super) async fn persist_candidate_targets(
        &self,
        candidate: &NotificationCandidate,
        targets: &[NotificationOutboxTarget],
        context: &Element,
        rich: &RichSummary,
        now_ms: i64,
    ) -> Result<bool, NotificationOutboxError> {
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_candidate_ancestry_tx(&mut tx, candidate).await?;
        let claimed = mark_candidate_outboxed_tx(&mut tx, candidate, now_ms).await?;
        if claimed == 0 {
            tx.commit().await?;
            return Ok(false);
        }
        for target in targets {
            enqueue_outbox_job_tx(&mut tx, candidate, target, context, rich, now_ms).await?;
        }
        settle_candidate_ancestry_tx(&mut tx, candidate).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub(super) async fn persist_exhausted_candidate(
        &self,
        candidate: &NotificationCandidate,
        next_policy_error_count: i64,
        now_ms: i64,
    ) -> Result<bool, NotificationOutboxError> {
        let reason = SuppressedReason::PolicyRetriesExhausted;
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        lock_candidate_ancestry_tx(&mut tx, candidate).await?;
        tx.execute(
            r#"
            UPDATE notification_candidates
            SET policy_error_count = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND sender_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
              AND quarantined_at_ms IS NULL
            "#,
            crate::db_params![
                next_policy_error_count,
                candidate.recipient_bare_jid.to_string(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.archive_stanza_id.by.to_string(),
                candidate.archive_stanza_id.id.clone(),
                candidate.class.as_db_value(),
            ],
        )
        .await?;
        record_candidate_suppressed_reason_tx(&mut tx, candidate, reason).await?;
        let claimed = mark_candidate_outboxed_tx(&mut tx, candidate, now_ms).await?;
        if claimed > 0 {
            settle_candidate_ancestry_tx(&mut tx, candidate).await?;
        }
        tx.commit().await?;
        Ok(claimed > 0)
    }

    pub(super) async fn persist_policy_retry(
        &self,
        candidate: &NotificationCandidate,
        next_policy_error_count: i64,
        now_ms: i64,
    ) -> Result<(), NotificationOutboxError> {
        self.execute(
            r#"
            UPDATE notification_candidates
            SET policy_error_count = ?,
                next_attempt_at_ms = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND sender_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
                  AND quarantined_at_ms IS NULL
            "#,
            crate::db_params![
                next_policy_error_count,
                now_ms.saturating_add(policy_retry_delay_ms(next_policy_error_count)),
                candidate.recipient_bare_jid.to_string(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.archive_stanza_id.by.to_string(),
                candidate.archive_stanza_id.id.clone(),
                candidate.class.as_db_value(),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn pending_candidates(
        &self,
        batch_size: usize,
    ) -> Result<Vec<NotificationCandidate>, NotificationOutboxError> {
        let batch_size = batch_size.clamp(1, 1_000);
        let mut rows = self
            .query(
                r#"
                SELECT recipient_bare_jid,
                       conversation_jid,
                       sender_jid,
                       thread_id,
                       stanza_id_by,
                       stanza_id,
                       class,
                       reason,
                       policy_error_count,
                       noping,
                       no_store,
                       no_permanent_store,
                       last_message_body,
                       reaction,
                       delivery_id
                FROM notification_candidates
                WHERE outboxed_at_ms IS NULL
                  AND quarantined_at_ms IS NULL
                  AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?)
                ORDER BY created_at_ms ASC,
                         recipient_bare_jid ASC,
                         conversation_jid ASC,
                         sender_jid ASC,
                         thread_id ASC,
                         stanza_id_by ASC,
                         stanza_id ASC,
                         class ASC
                LIMIT ?
                "#,
                crate::db_params![crate::time::now_ms(), batch_size as i64],
            )
            .await?;
        let mut candidates = Vec::new();
        while let Some(row) = rows.next().await? {
            match decode_candidate(&row) {
                Ok(candidate) => candidates.push(candidate),
                Err(error) => {
                    self.mark_malformed_candidate_outboxed(&row, &error).await?;
                }
            }
        }
        Ok(candidates)
    }

    async fn mark_malformed_candidate_outboxed(
        &self,
        row: &Row,
        _error: &NotificationOutboxError,
    ) -> Result<(), NotificationOutboxError> {
        let recipient_raw: String = row.get(0)?;
        let conversation_raw: String = row.get(1)?;
        let thread_id: String = row.get(3)?;
        let stanza_id_by_raw: String = row.get(4)?;
        let stanza_id: String = row.get(5)?;
        let class: String = row.get(6)?;
        tracing::warn!("dropping malformed XEP-0357 notification candidate fail-closed");
        let id = row
            .get::<Option<String>>(14)?
            .map(|raw| uuid::Uuid::parse_str(&raw))
            .transpose()
            .map_err(|_| NotificationOutboxError::InvalidDeliveryIdentity)?;
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        if let Some(id) = id {
            EffectDescendantRepository::lock_all_nowait_raw(&mut tx, id).await?;
        }
        let changed = tx
            .execute(
                r#"
            UPDATE notification_candidates
            SET outboxed_at_ms = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
                  AND quarantined_at_ms IS NULL
            "#,
                crate::db_params![
                    crate::time::now_ms(),
                    recipient_raw,
                    conversation_raw,
                    thread_id,
                    stanza_id_by_raw,
                    stanza_id,
                    class,
                ],
            )
            .await?;
        if changed > 0 {
            if let Some(id) = id {
                EffectDescendantRepository::settle_all_raw(&mut tx, id, chrono::Utc::now()).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

pub(super) async fn enqueue_outbox_job_tx(
    tx: &mut EnteredTransaction<'_>,
    candidate: &NotificationCandidate,
    target: &NotificationOutboxTarget,
    context: &Element,
    rich: &RichSummary,
    now_ms: i64,
) -> Result<(), NotificationOutboxError> {
    // The durable schema stores XML as TEXT; keep protocol context typed until this DB write edge.
    lock_candidate_ancestry_tx(tx, candidate).await?;
    let context_xml = String::from(context);
    for _ in 0..8 {
        let inserted =
            insert_outbox_job_tx(tx, candidate, target, context_xml.as_str(), rich, now_ms).await?;
        if inserted > 0 {
            return Ok(());
        }
        match merge_outbox_job_tx(tx, candidate, target, context_xml.as_str(), rich, now_ms).await?
        {
            OutboxMergeOutcome::Merged => return Ok(()),
            OutboxMergeOutcome::MalformedExistingJobFailed
            | OutboxMergeOutcome::QueuedJobNotFound
            | OutboxMergeOutcome::QueuedJobChanged => {}
        }
    }
    Err(NotificationOutboxError::OutboxCoalesceContention)
}

async fn insert_outbox_job_tx(
    tx: &mut EnteredTransaction<'_>,
    candidate: &NotificationCandidate,
    target: &NotificationOutboxTarget,
    context_xml: &str,
    rich: &RichSummary,
    now_ms: i64,
) -> Result<u64, NotificationOutboxError> {
    let job_id = NotificationOutboxJobId::fresh();
    let sender_jids = encode_sender_jids(std::slice::from_ref(&candidate.sender_jid))?;
    let changed = tx
        .execute(
            r#"
            INSERT INTO notification_outbox (
                job_id,
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
                summary_sender_jid,
                summary_body,
                status,
                attempt_count,
                policy_error_count,
                last_error,
                next_attempt_at_ms,
                claimed_at_ms,
                claim_token,
                created_at_ms,
                updated_at_ms,
                published_at_ms,
                queue_acceptance_may_exist
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, 0, 0, NULL, NULL, NULL, NULL, ?, ?, NULL, 0)
            ON CONFLICT DO NOTHING
            "#,
            crate::db_params![
                job_id.as_str(),
                candidate.recipient_bare_jid.to_string(),
                target.push_service_jid.to_string(),
                target.node.as_str(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                sender_jids,
                candidate.thread_id.as_str(),
                candidate.class.as_db_value(),
                context_xml,
                rich.sender.as_ref().map(ToString::to_string),
                rich.body.clone(),
                STATUS_QUEUED,
                now_ms,
                now_ms,
            ],
        )
        .await?;
    if changed > 0 {
        copy_candidate_ancestry_tx(tx, candidate, job_id.as_str()).await?;
    }
    Ok(changed)
}

enum OutboxMergeOutcome {
    Merged,
    MalformedExistingJobFailed,
    QueuedJobNotFound,
    QueuedJobChanged,
}

async fn merge_outbox_job_tx(
    tx: &mut EnteredTransaction<'_>,
    candidate: &NotificationCandidate,
    target: &NotificationOutboxTarget,
    context_xml: &str,
    rich: &RichSummary,
    now_ms: i64,
) -> Result<OutboxMergeOutcome, NotificationOutboxError> {
    let mut rows = tx
        .query(
            r#"
            SELECT job_id, sender_jid, sender_jids
            FROM notification_outbox
            WHERE recipient_bare_jid = ?
              AND push_service_jid = ?
              AND node = ?
              AND conversation_jid = ?
              AND thread_id = ?
              AND class = ?
              AND status = ?
              AND approved_payload_xml IS NULL
              AND queue_acceptance_may_exist = 0
            LIMIT 1
            "#,
            crate::db_params![
                candidate.recipient_bare_jid.to_string(),
                target.push_service_jid.to_string(),
                target.node.as_str(),
                candidate.conversation_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.class.as_db_value(),
                STATUS_QUEUED,
            ],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(OutboxMergeOutcome::QueuedJobNotFound);
    };
    let job_id_raw: String = row.get(0)?;
    let sender_raw = row
        .get::<Option<String>>(1)?
        .ok_or_else(|| NotificationOutboxError::InvalidSenderJid("<null>".to_string()));
    let sender_jids_raw = row
        .get::<Option<String>>(2)?
        .ok_or(NotificationOutboxError::MissingSenderJidSet);
    let existing_sender_jid = match sender_raw.and_then(|raw| {
        let sender_jid = raw
            .parse()
            .map_err(|_| NotificationOutboxError::InvalidSenderJid(raw))?;
        require_full_sender_jid(&sender_jid)?;
        require_sender_matches_conversation(&sender_jid, &candidate.conversation_jid)?;
        Ok(sender_jid)
    }) {
        Ok(sender_jid) => sender_jid,
        Err(error) => {
            mark_malformed_outbox_job_failed_tx(
                tx,
                job_id_raw.as_str(),
                &error.to_string(),
                now_ms,
            )
            .await?;
            return Ok(OutboxMergeOutcome::MalformedExistingJobFailed);
        }
    };
    let mut sender_jids = match sender_jids_raw.and_then(|raw| {
        let sender_jids = decode_sender_jids(&raw)?;
        require_full_sender_jid_set(&sender_jids)?;
        require_sender_set_matches_conversation(&sender_jids, &candidate.conversation_jid)?;
        require_sender_set_contains_scalar(&sender_jids, &existing_sender_jid)?;
        Ok(sender_jids)
    }) {
        Ok(sender_jids) => sender_jids,
        Err(error) => {
            mark_malformed_outbox_job_failed_tx(
                tx,
                job_id_raw.as_str(),
                &error.to_string(),
                now_ms,
            )
            .await?;
            return Ok(OutboxMergeOutcome::MalformedExistingJobFailed);
        }
    };
    if !sender_jids
        .iter()
        .any(|sender_jid| sender_jid == &candidate.sender_jid)
    {
        sender_jids.push(candidate.sender_jid.clone());
    }
    let sender_jids = encode_sender_jids(&sender_jids)?;
    let affected = tx
        .execute(
            r#"
        UPDATE notification_outbox
        SET message_count = message_count + 1,
            context_xml = ?,
            sender_jid = ?,
            sender_jids = ?,
            summary_sender_jid = ?,
            summary_body = ?,
            policy_error_count = 0,
            last_error = NULL,
            next_attempt_at_ms = NULL,
            updated_at_ms = ?
        WHERE job_id = ?
          AND status = ?
          AND approved_payload_xml IS NULL
              AND queue_acceptance_may_exist = 0
        "#,
            crate::db_params![
                context_xml,
                candidate.sender_jid.to_string(),
                sender_jids,
                rich.sender.as_ref().map(ToString::to_string),
                rich.body.clone(),
                now_ms,
                job_id_raw.clone(),
                STATUS_QUEUED,
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(OutboxMergeOutcome::QueuedJobChanged);
    }
    copy_candidate_ancestry_tx(tx, candidate, &job_id_raw).await?;
    Ok(OutboxMergeOutcome::Merged)
}

async fn mark_malformed_outbox_job_failed_tx(
    tx: &mut EnteredTransaction<'_>,
    job_id: &str,
    error: &str,
    now_ms: i64,
) -> Result<(), NotificationOutboxError> {
    lock_outbox_ancestry_tx(tx, job_id).await?;
    let changed = tx
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
        "#,
            crate::db_params![
                STATUS_FAILED,
                format!("malformed notification outbox job: {error}"),
                now_ms,
                job_id,
                STATUS_QUEUED,
            ],
        )
        .await?;
    if changed > 0 {
        settle_outbox_if_unowned_tx(tx, job_id).await?;
    }
    Ok(())
}

pub(super) async fn mark_candidate_outboxed_tx(
    tx: &mut Transaction<'_>,
    candidate: &NotificationCandidate,
    now_ms: i64,
) -> Result<u64, NotificationOutboxError> {
    Ok(tx
        .execute(
            r#"
            UPDATE notification_candidates
            SET outboxed_at_ms = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND sender_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
                  AND quarantined_at_ms IS NULL
            "#,
            crate::db_params![
                now_ms,
                candidate.recipient_bare_jid.to_string(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.archive_stanza_id.by.to_string(),
                candidate.archive_stanza_id.id.clone(),
                candidate.class.as_db_value(),
            ],
        )
        .await?)
}

/// Records a typed [`SuppressedReason`] onto a not-yet-outboxed
/// candidate row inside an active transaction. Always called BEFORE
/// [`mark_candidate_outboxed_tx`] in the T1 suppression path so the
/// `suppressed_reason` column persists for the row's lifetime in the
/// outboxed-prune retention window.
pub(super) async fn record_candidate_suppressed_reason_tx(
    tx: &mut Transaction<'_>,
    candidate: &NotificationCandidate,
    reason: SuppressedReason,
) -> Result<u64, NotificationOutboxError> {
    Ok(tx
        .execute(
            r#"
            UPDATE notification_candidates
            SET suppressed_reason = ?
            WHERE recipient_bare_jid = ?
              AND conversation_jid = ?
              AND sender_jid = ?
              AND thread_id = ?
              AND stanza_id_by = ?
              AND stanza_id = ?
              AND class = ?
              AND outboxed_at_ms IS NULL
                  AND quarantined_at_ms IS NULL
            "#,
            crate::db_params![
                reason.as_db_value(),
                candidate.recipient_bare_jid.to_string(),
                candidate.conversation_jid.to_string(),
                candidate.sender_jid.to_string(),
                candidate.thread_id.as_str(),
                candidate.archive_stanza_id.by.to_string(),
                candidate.archive_stanza_id.id.clone(),
                candidate.class.as_db_value(),
            ],
        )
        .await?)
}

async fn candidate_delivery_id_tx(
    tx: &mut Transaction<'_>,
    candidate: &NotificationCandidate,
) -> Result<Option<uuid::Uuid>, NotificationOutboxError> {
    match candidate.delivery_id {
        Some(id) => Ok(Some(id)),
        None => Ok(
            NotificationOutboxStore::candidate_delivery_id_in_transaction(tx, candidate).await?,
        ),
    }
}

pub(super) async fn lock_candidate_ancestry_tx(
    tx: &mut EnteredTransaction<'_>,
    candidate: &NotificationCandidate,
) -> Result<(), NotificationOutboxError> {
    if let Some(id) = candidate_delivery_id_tx(tx, candidate).await? {
        EffectDescendantRepository::lock_all_raw(tx, id).await?;
    }
    Ok(())
}

async fn copy_candidate_ancestry_tx(
    tx: &mut EnteredTransaction<'_>,
    candidate: &NotificationCandidate,
    job_id: &str,
) -> Result<(), NotificationOutboxError> {
    if let Some(source) = candidate_delivery_id_tx(tx, candidate).await? {
        let target = uuid::Uuid::parse_str(job_id)
            .map_err(|_| NotificationOutboxError::InvalidDeliveryIdentity)?;
        EffectDescendantRepository::copy_raw(tx, source, target).await?;
        tx.execute("INSERT INTO notification_outbox_lineage (candidate_delivery_id, job_id) VALUES (?, ?) ON CONFLICT DO NOTHING",
            crate::db_params![source.to_string(), target.to_string()]).await?;
    }
    Ok(())
}

pub(super) async fn settle_candidate_ancestry_tx(
    tx: &mut EnteredTransaction<'_>,
    candidate: &NotificationCandidate,
) -> Result<(), NotificationOutboxError> {
    if let Some(id) = candidate_delivery_id_tx(tx, candidate).await? {
        EffectDescendantRepository::settle_all_raw(tx, id, chrono::Utc::now()).await?;
    }
    Ok(())
}
