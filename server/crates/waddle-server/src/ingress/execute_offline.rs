//! Atomic ordinary pending delivery and frozen notification settlement.
use waddle_xmpp::{
    ingress::{
        IngressEffectIntent, NotificationActivityMutation, NotificationCandidateOutcome,
        PendingDeliveryMutation,
    },
    pending_delivery::{InsertOutcome, PendingPayload, PendingRow, QuotaPolicy},
};

use crate::ingress::decision::{EffectReceiptKey, IngressDecision};
use crate::{
    ingress_uow::{
        settle_recorded, CanonicalMessageRepository, EffectIntentRepository,
        EffectReceiptRepository, IngressUnitOfWork, IngressUowError, IngressUowTransaction,
        PendingReceiptRepository, RecoveryReceiptRepository,
    },
    notification_outbox::NotificationCandidateInsertOutcome,
    server::routes::interpret::{
        effects::{
            delivery::{ExternalDeliveryEffect, PreparedOfflineNotification},
            EffectOutcome, SettledCompletion, SettledOutcome,
        },
        Deps,
    },
};

#[derive(Debug, thiserror::Error)]
enum StoreError {
    #[error(transparent)]
    Ingress(#[from] IngressUowError),
    #[error(transparent)]
    Pending(#[from] waddle_xmpp::pending_delivery::storage::PendingStorageError),
}

enum StoreOutcome {
    Settled(SettledOutcome),
    QuotaExceeded(SettledOutcome),
}

#[cfg(test)]
static FAIL_BEFORE_SETTLEMENT: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<waddle_xmpp::ingress::MessageKey>>,
> = std::sync::LazyLock::new(Default::default);

#[cfg(test)]
pub(crate) fn fail_before_offline_settlement(key: waddle_xmpp::ingress::MessageKey) {
    FAIL_BEFORE_SETTLEMENT
        .lock()
        .expect("offline fault hooks")
        .insert(key);
}

pub(super) async fn execute(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalDeliveryEffect,
    deps: &Deps<'_>,
) -> EffectOutcome {
    let ExternalDeliveryEffect::QueueOfflineDelivery {
        row,
        prepared_notification,
        original_message,
    } = effect
    else {
        return EffectOutcome::Unavailable;
    };
    let Some(storage) = deps.pending_delivery_storage else {
        return EffectOutcome::Unavailable;
    };
    match store(
        uow,
        decision,
        index,
        row,
        prepared_notification,
        storage.quota_policy(),
    )
    .await
    {
        Ok(StoreOutcome::Settled(settled)) => EffectOutcome::Settled(settled),
        Ok(StoreOutcome::QuotaExceeded(settled)) => {
            crate::server::routes::interpret::offline_delivery::bounce_offline_quota(
                deps,
                &row.recipient,
                original_message,
            )
            .await;
            EffectOutcome::Settled(settled)
        }
        Err(error) => {
            tracing::warn!(%error, "offline delivery settlement failed");
            EffectOutcome::Unavailable
        }
    }
}

async fn store(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    row: &PendingRow,
    prepared: &PreparedOfflineNotification,
    quota: QuotaPolicy,
) -> Result<StoreOutcome, StoreError> {
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
        return Err(IngressUowError::EffectIntentMessageMissing.into());
    }
    let pending = pending_intent(row);
    let receipt = crate::ingress::receipt_key(&pending)?;
    let already_receipted = EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await?;
    if already_receipted
        && owned_receipts_complete(&mut tx, key, &decision.external_receipts[index]).await?
    {
        // A settled refusal has no pending row or candidate to recreate.
        tx.commit().await?;
        return Ok(StoreOutcome::Settled(SettledOutcome {
            persisted: Vec::new(),
            completion: SettledCompletion::Complete,
            detached: None,
        }));
    }
    if !already_receipted && !PendingReceiptRepository::contains(&mut tx, &row.id).await? {
        match PendingReceiptRepository::insert(&mut tx, row, quota).await? {
            InsertOutcome::Inserted => {}
            InsertOutcome::QuotaExceeded => {
                let mut evidence = Vec::new();
                for intent in EffectIntentRepository::load(&mut tx, key).await? {
                    if decision.external_receipts[index]
                        .contains(&crate::ingress::receipt_key(&intent)?)
                    {
                        evidence.push(intent);
                    }
                }
                let persisted = settle_recorded(&mut tx, key, &evidence).await?;
                // Refusal resolves the pending delivery and its notification previews.
                // Commit before the keyless sender bounce: a crash between commit and
                // bounce loses the bounce (at-most-once), the RFC's trade for keyless
                // sinks. Bouncing first would repeat the refusal on every retry.
                tx.commit().await?;
                return Ok(StoreOutcome::QuotaExceeded(SettledOutcome {
                    persisted,
                    completion: SettledCompletion::Complete,
                    detached: None,
                }));
            }
        }
    }
    let mut evidence = if already_receipted {
        Vec::new()
    } else {
        vec![pending.clone()]
    };
    notification_evidence(&mut tx, decision, index, row, prepared, &mut evidence).await?;
    #[cfg(test)]
    if FAIL_BEFORE_SETTLEMENT
        .lock()
        .expect("offline fault hooks")
        .remove(&key)
    {
        return Err(IngressUowError::Timeout.into());
    }
    let persisted = settle_recorded(&mut tx, key, &evidence).await?;
    if !already_receipted && !persisted.contains(&pending) {
        return Err(IngressUowError::EffectIntentConflict.into());
    }
    let complete =
        owned_receipts_complete(&mut tx, key, &decision.external_receipts[index]).await?;
    tx.commit().await?;
    Ok(StoreOutcome::Settled(SettledOutcome {
        persisted,
        completion: if complete {
            SettledCompletion::Complete
        } else {
            SettledCompletion::Incomplete
        },
        detached: None,
    }))
}

async fn owned_receipts_complete(
    tx: &mut IngressUowTransaction<'_>,
    key: waddle_xmpp::ingress::MessageKey,
    receipts: &[EffectReceiptKey],
) -> Result<bool, IngressUowError> {
    for receipt in receipts {
        if !EffectReceiptRepository::contains(
            tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn pending_intent(row: &PendingRow) -> IngressEffectIntent {
    IngressEffectIntent::PendingDelivery {
        mutation: match &row.payload {
            PendingPayload::Archived(archive_stanza_id) => PendingDeliveryMutation::Archived {
                recipient: row.recipient.clone(),
                row_id: row.id.clone(),
                archive_stanza_id: archive_stanza_id.clone(),
            },
            PendingPayload::Transient(_) => PendingDeliveryMutation::Transient {
                recipient: row.recipient.clone(),
                row_id: row.id.clone(),
            },
        },
    }
}

async fn notification_evidence(
    tx: &mut IngressUowTransaction<'_>,
    decision: &IngressDecision,
    index: usize,
    row: &PendingRow,
    prepared: &PreparedOfflineNotification,
    evidence: &mut Vec<IngressEffectIntent>,
) -> Result<(), StoreError> {
    let PendingPayload::Archived(archive_stanza_id) = &row.payload else {
        return Ok(());
    };
    match prepared {
        PreparedOfflineNotification::Prepared(candidate) => {
            if candidate.recipient_bare_jid() != &row.recipient
                || candidate.archive_stanza_id() != archive_stanza_id
            {
                return Err(IngressUowError::EffectIntentConflict.into());
            }
            let outcome = match RecoveryReceiptRepository::insert_candidate(tx, candidate).await? {
                NotificationCandidateInsertOutcome::Inserted => {
                    NotificationCandidateOutcome::Inserted
                }
                NotificationCandidateInsertOutcome::Duplicate => {
                    NotificationCandidateOutcome::Duplicate
                }
            };
            evidence.push(IngressEffectIntent::NotificationActivityPreview {
                owner: row.recipient.clone(),
                mutation: NotificationActivityMutation::NotificationCandidate {
                    conversation: row.recipient.clone(),
                    archive_stanza_id: archive_stanza_id.clone(),
                    outcome,
                },
            });
        }
        PreparedOfflineNotification::Suppressed => {}
        PreparedOfflineNotification::RetryLater => return Ok(()),
    }
    let marker = IngressEffectIntent::NotificationActivityPreview {
        owner: row.recipient.clone(),
        mutation: NotificationActivityMutation::OfflineDelivery {
            conversation: row.recipient.clone(),
            archive_stanza_id: archive_stanza_id.clone(),
        },
    };
    if decision.external_receipts[index].contains(&crate::ingress::receipt_key(&marker)?) {
        PendingReceiptRepository::mark_notification_outboxed(tx, &row.id).await?;
        evidence.push(marker);
    }
    Ok(())
}
