//! Commit notification work and its recorded obligations under one canonical lock.
use crate::{
    ingress::decision::IngressDecision,
    ingress_uow::{
        settle_recorded, CanonicalMessageRepository, EffectReceiptRepository, IngressUnitOfWork,
        IngressUowError, RecoveryCompletion, RecoveryReceiptRepository,
    },
    notification_outbox::NotificationCandidateInsertOutcome,
    server::routes::interpret::effects::{
        room::ExternalRoomEffect, EffectOutcome, SettledCompletion, SettledOutcome,
    },
};
use waddle_xmpp::ingress::{
    GroupchatNotificationRecoveryAction, GroupchatNotificationRecoveryMutation,
    IngressEffectIntent, NotificationActivityMutation, NotificationCandidateOutcome,
};

#[cfg(test)]
static FAIL_AFTER_UPDATE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<waddle_xmpp::ingress::MessageKey>>,
> = std::sync::LazyLock::new(Default::default);

#[cfg(test)]
pub(crate) fn fail_after_recovery_update(key: waddle_xmpp::ingress::MessageKey) {
    FAIL_AFTER_UPDATE
        .lock()
        .expect("recovery fault hooks")
        .insert(key);
}

pub(super) async fn execute(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalRoomEffect,
) -> EffectOutcome {
    match store(uow, decision, index, effect).await {
        Ok(outcome) => EffectOutcome::Settled(outcome),
        Err(error) => {
            tracing::warn!(%error, "notification recovery settlement failed");
            EffectOutcome::Unavailable
        }
    }
}

async fn store(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalRoomEffect,
) -> Result<SettledOutcome, IngressUowError> {
    let ExternalRoomEffect::NotificationCandidate {
        owner,
        room,
        archive_stanza_id,
        candidate,
        recovery,
    } = effect
    else {
        return Err(IngressUowError::EffectIntentMessageMissing);
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
    let mut evidence = Vec::new();
    if let Some(candidate) = candidate {
        if candidate.recipient_bare_jid() != owner
            || candidate.conversation_jid() != room
            || candidate.archive_stanza_id() != archive_stanza_id
        {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let created_at_ms = recovery
            .as_ref()
            .map_or_else(crate::time::now_ms, |recovery| recovery.created_at_ms);
        let outcome =
            match RecoveryReceiptRepository::insert_candidate(&mut tx, candidate, created_at_ms)
                .await?
            {
                NotificationCandidateInsertOutcome::Inserted => {
                    NotificationCandidateOutcome::Inserted
                }
                NotificationCandidateInsertOutcome::Duplicate => {
                    NotificationCandidateOutcome::Duplicate
                }
            };
        evidence.push(IngressEffectIntent::NotificationActivityPreview {
            owner: owner.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: room.clone(),
                archive_stanza_id: archive_stanza_id.clone(),
                outcome,
            },
        });
    }
    if let Some(recovery) = recovery {
        if &recovery.key.recipient != owner
            || &recovery.key.room != room
            || &recovery.key.archive_stanza_id != archive_stanza_id
        {
            return Err(IngressUowError::EffectIntentConflict);
        }
        match RecoveryReceiptRepository::complete(&mut tx, key, &recovery.key).await? {
            RecoveryCompletion::Missing => return Err(IngressUowError::EffectIntentMessageMissing),
            RecoveryCompletion::Completed | RecoveryCompletion::AlreadyCompleted => {}
        }
        #[cfg(test)]
        if FAIL_AFTER_UPDATE
            .lock()
            .expect("recovery fault hooks")
            .remove(&key)
        {
            return Err(IngressUowError::Timeout);
        }
        evidence.push(IngressEffectIntent::GroupchatNotificationRecovery {
            mutation: GroupchatNotificationRecoveryMutation {
                recipient: recovery.key.recipient.clone(),
                room: recovery.key.room.clone(),
                thread_id: recovery
                    .key
                    .thread_id
                    .clone()
                    .map(|thread| {
                        waddle_xmpp_core::mam::ThreadId::new(thread)
                            .ok_or(IngressUowError::EffectIntentConflict)
                    })
                    .transpose()?,
                archive_stanza_id: recovery.key.archive_stanza_id.clone(),
                sender: recovery.sender_jid.clone(),
                is_live_occupant: recovery.is_live_occupant,
                room_members_only: recovery.room_members_only,
                sender_can_broadcast_channel_mention: recovery.sender_can_broadcast_channel_mention,
                created_at_ms: recovery.created_at_ms,
                action: GroupchatNotificationRecoveryAction::Completed,
            },
        });
    }
    let persisted = settle_recorded(&mut tx, key, &evidence).await?;
    let mut complete = true;
    for receipt in &decision.external_receipts[index] {
        if !EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await?
        {
            complete = false;
        }
    }
    tx.commit().await?;
    Ok(SettledOutcome {
        persisted,
        completion: if complete {
            SettledCompletion::Complete
        } else {
            SettledCompletion::Incomplete
        },
        detached: None,
    })
}
