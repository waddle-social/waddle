//! Commit a frozen live-recipient call candidate and its receipt atomically.
use crate::ingress::decision::IngressDecision;
use crate::ingress_uow::{
    run_with_retry, settle_recorded, CanonicalMessageRepository, EffectReceiptRepository,
    IngressUnitOfWork, IngressUowError, RecoveryReceiptRepository,
};
use crate::notification_outbox::NotificationCandidateInsertOutcome;
use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
use crate::server::routes::interpret::effects::{EffectOutcome, SettledCompletion, SettledOutcome};
use waddle_xmpp::ingress::{
    IngressEffectIntent, NotificationActivityMutation, NotificationCandidateOutcome,
};

#[cfg(test)]
tokio::task_local! {
    static LOSE_COMMIT_ACK: bool;
}

pub(super) async fn execute(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalDeliveryEffect,
) -> EffectOutcome {
    match run_with_retry(5, || store(uow, decision, index, effect)).await {
        Ok(outcome) => EffectOutcome::Settled(outcome),
        Err(failure) => {
            tracing::warn!(error = %failure.last_error, "live call notification settlement failed");
            EffectOutcome::Settled(SettledOutcome {
                refusal: None,
                persisted: Vec::new(),
                completion: SettledCompletion::Uncertain,
                detached: None,
            })
        }
    }
}

async fn store(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalDeliveryEffect,
) -> Result<SettledOutcome, IngressUowError> {
    let ExternalDeliveryEffect::NotificationCandidate { candidate } = effect else {
        return Err(IngressUowError::EffectIntentConflict);
    };
    let key = decision
        .message_key
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    let mut tx = uow
        .begin_with_timeouts(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(250),
        )
        .await?;
    if !CanonicalMessageRepository::lock(&mut tx, key).await? {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: candidate.recipient_bare_jid().clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            // DM receipts follow the offline path's recipient-owned shape.
            conversation: candidate.recipient_bare_jid().clone(),
            archive_stanza_id: candidate.archive_stanza_id().clone(),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let receipt = crate::ingress::receipt_key(&intent)?;
    if decision.external_receipts[index].as_slice() != std::slice::from_ref(&receipt) {
        return Err(IngressUowError::EffectIntentConflict);
    }
    // Replaying a settled effect must not recreate a candidate already drained
    // from the outbox, or one maintenance deliberately expired.
    if EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await?
    {
        tx.commit().await?;
        return Ok(SettledOutcome {
            refusal: None,
            persisted: Vec::new(),
            completion: SettledCompletion::Complete,
            detached: None,
        });
    }
    let created_at = CanonicalMessageRepository::created_at(&mut tx, key).await?;
    let outcome = match RecoveryReceiptRepository::insert_candidate(
        &mut tx,
        key,
        candidate,
        created_at.timestamp_millis(),
    )
    .await?
    {
        NotificationCandidateInsertOutcome::Inserted => NotificationCandidateOutcome::Inserted,
        NotificationCandidateInsertOutcome::Duplicate => NotificationCandidateOutcome::Duplicate,
    };
    let evidence = IngressEffectIntent::NotificationActivityPreview {
        owner: candidate.recipient_bare_jid().clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: candidate.recipient_bare_jid().clone(),
            archive_stanza_id: candidate.archive_stanza_id().clone(),
            outcome,
        },
    };
    let persisted = settle_recorded(&mut tx, key, &[evidence]).await?;
    if !EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await?
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    tx.commit().await?;
    // Model a successful commit whose acknowledgement never reaches the arm.
    #[cfg(test)]
    if LOSE_COMMIT_ACK.try_with(|lose| *lose).unwrap_or(false) {
        return Err(IngressUowError::Timeout);
    }
    Ok(SettledOutcome {
        refusal: None,
        persisted,
        completion: SettledCompletion::Complete,
        detached: None,
    })
}

#[cfg(test)]
#[path = "execute_notification_tests.rs"]
mod tests;
