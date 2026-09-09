//! Reconcile notification obligations without exposing ingress transactions.
use super::IngressAuthority;
use crate::{
    ingress_substrate::MessageEnvelope,
    ingress_uow::{
        settle_recorded, CanonicalMessageRepository, EffectIntentRepository,
        EffectReceiptRepository, IngressUowError, IngressUowTransaction, RecoveryCompletion,
        RecoveryReceiptRepository,
    },
    notification_outbox::{NotificationCandidate, NotificationCandidateInsertOutcome},
};
use waddle_xmpp::{
    inbox::storage::GroupchatNotificationRecovery,
    ingress::{
        GroupchatNotificationRecoveryAction, IngressEffectIntent, NotificationActivityMutation,
        NotificationCandidateOutcome,
    },
};

/// Frozen work read under the canonical lock. Policy evaluation happens only after release.
#[derive(Debug)]
pub enum RecoveryPreparation {
    CanonicalGone,
    Ready {
        envelope: Box<MessageEnvelope>,
        deferred_policy: bool,
        candidate_required: bool,
        completed: bool,
    },
}

#[derive(Debug)]
pub enum RecoveryPolicyDecision {
    Deliver(Box<NotificationCandidate>),
    Suppressed,
    RetryLater,
    AlreadyCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverySweepOutcome {
    Completed,
    Pending,
    Missing,
    CanonicalGone,
}

impl IngressAuthority {
    pub async fn prepare_notification_recovery(
        &self,
        recovery: &GroupchatNotificationRecovery,
    ) -> Result<RecoveryPreparation, IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let mut tx = self
            .uow
            .begin_with_timeouts(
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(250),
            )
            .await?;
        if !CanonicalMessageRepository::lock(&mut tx, recovery.message_key).await? {
            RecoveryReceiptRepository::delete(&mut tx, recovery.message_key, &recovery.key).await?;
            tx.commit().await?;
            return Ok(RecoveryPreparation::CanonicalGone);
        }
        let envelope = CanonicalMessageRepository::load_envelope(&mut tx, recovery.message_key)
            .await?
            .ok_or(IngressUowError::EffectIntentMessageMissing)?;
        let completed =
            RecoveryReceiptRepository::is_completed(&mut tx, recovery.message_key, &recovery.key)
                .await?;
        let intents = unreceipted(&mut tx, recovery).await?;
        let deferred_policy = intents.iter().any(|intent| matches!(intent, IngressEffectIntent::GroupchatNotificationRecovery { mutation } if mutation.action == GroupchatNotificationRecoveryAction::DeferredPolicy));
        let candidate_required = intents.iter().any(is_candidate);
        tx.commit().await?;
        Ok(RecoveryPreparation::Ready {
            envelope: Box::new(envelope),
            deferred_policy,
            candidate_required,
            completed,
        })
    }

    /// Apply previously prepared, typed policy evidence after revalidating recorded obligations.
    /// The extra argument keeps policy stores and occupant-secret handling outside this authority.
    pub async fn settle_notification_recovery(
        &self,
        recovery: &GroupchatNotificationRecovery,
        policy: RecoveryPolicyDecision,
    ) -> Result<RecoverySweepOutcome, IngressUowError> {
        if matches!(policy, RecoveryPolicyDecision::RetryLater) {
            return Ok(RecoverySweepOutcome::Pending);
        }
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let mut tx = self
            .uow
            .begin_with_timeouts(
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(250),
            )
            .await?;
        if !CanonicalMessageRepository::lock(&mut tx, recovery.message_key).await? {
            RecoveryReceiptRepository::delete(&mut tx, recovery.message_key, &recovery.key).await?;
            tx.commit().await?;
            return Ok(RecoverySweepOutcome::CanonicalGone);
        }
        let recorded = EffectIntentRepository::load(&mut tx, recovery.message_key).await?;
        if ![
            GroupchatNotificationRecoveryAction::Completed,
            GroupchatNotificationRecoveryAction::DeferredPolicy,
        ]
        .iter()
        .any(|action| recorded.contains(&recovery_intent(recovery, *action)))
        {
            return Ok(RecoverySweepOutcome::Missing);
        }
        let intents = unreceipted(&mut tx, recovery).await?;
        let completed =
            RecoveryReceiptRepository::is_completed(&mut tx, recovery.message_key, &recovery.key)
                .await?;
        let candidate_required = intents.iter().any(is_candidate);
        if completed && candidate_required {
            return Ok(RecoverySweepOutcome::Pending);
        }
        let deferred = intents.iter().any(|intent| matches!(intent, IngressEffectIntent::GroupchatNotificationRecovery { mutation } if mutation.action == GroupchatNotificationRecoveryAction::DeferredPolicy));
        let mut evidence = Vec::new();
        if !completed && (candidate_required || deferred) {
            match &policy {
                RecoveryPolicyDecision::Deliver(candidate) => {
                    if candidate.recipient_bare_jid() != &recovery.key.recipient
                        || candidate.conversation_jid() != &recovery.key.room
                        || candidate.archive_stanza_id() != &recovery.key.archive_stanza_id
                        || candidate.sender_jid() != &recovery.sender_jid
                    {
                        return Ok(RecoverySweepOutcome::Missing);
                    }
                    let outcome =
                        match RecoveryReceiptRepository::insert_candidate(&mut tx, candidate)
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
                        owner: recovery.key.recipient.clone(),
                        mutation: NotificationActivityMutation::NotificationCandidate {
                            conversation: recovery.key.room.clone(),
                            archive_stanza_id: recovery.key.archive_stanza_id.clone(),
                            outcome,
                        },
                    });
                }
                RecoveryPolicyDecision::Suppressed if deferred && !candidate_required => {}
                _ => return Ok(RecoverySweepOutcome::Pending),
            }
        }
        if RecoveryReceiptRepository::complete(&mut tx, recovery.message_key, &recovery.key).await?
            == RecoveryCompletion::Missing
        {
            return Ok(RecoverySweepOutcome::Missing);
        }
        for intent in intents {
            if let IngressEffectIntent::GroupchatNotificationRecovery { mut mutation } = intent {
                mutation.action = GroupchatNotificationRecoveryAction::Completed;
                evidence.push(IngressEffectIntent::GroupchatNotificationRecovery { mutation });
            }
        }
        settle_recorded(&mut tx, recovery.message_key, &evidence).await?;
        super::execute::terminalize_if_complete_in_transaction(&mut tx, recovery.message_key)
            .await?;
        tx.commit().await?;
        Ok(RecoverySweepOutcome::Completed)
    }
}

fn is_candidate(intent: &IngressEffectIntent) -> bool {
    matches!(
        intent,
        IngressEffectIntent::NotificationActivityPreview {
            mutation: NotificationActivityMutation::NotificationCandidate {
                outcome: NotificationCandidateOutcome::Inserted,
                ..
            },
            ..
        }
    )
}

async fn unreceipted(
    tx: &mut IngressUowTransaction<'_>,
    recovery: &GroupchatNotificationRecovery,
) -> Result<Vec<IngressEffectIntent>, IngressUowError> {
    let mut pending = Vec::new();
    for intent in EffectIntentRepository::load(tx, recovery.message_key).await? {
        let matches = match &intent {
            IngressEffectIntent::GroupchatNotificationRecovery { mutation } => {
                mutation.recipient == recovery.key.recipient
                    && mutation.room == recovery.key.room
                    && mutation.archive_stanza_id == recovery.key.archive_stanza_id
                    && mutation.thread_id.as_ref().map(|thread| thread.as_str())
                        == recovery.key.thread_id.as_deref()
                    && mutation.sender == recovery.sender_jid
                    && mutation.is_live_occupant == recovery.is_live_occupant
                    && mutation.room_members_only == recovery.room_members_only
                    && mutation.sender_can_broadcast_channel_mention
                        == recovery.sender_can_broadcast_channel_mention
                    && mutation.created_at_ms == recovery.created_at_ms
                    && matches!(
                        mutation.action,
                        GroupchatNotificationRecoveryAction::Completed
                            | GroupchatNotificationRecoveryAction::DeferredPolicy
                    )
            }
            IngressEffectIntent::NotificationActivityPreview {
                owner,
                mutation:
                    NotificationActivityMutation::NotificationCandidate {
                        conversation,
                        archive_stanza_id,
                        ..
                    },
            } => {
                *owner == recovery.key.recipient
                    && *conversation == recovery.key.room
                    && *archive_stanza_id == recovery.key.archive_stanza_id
            }
            _ => false,
        };
        if matches {
            let receipt = super::receipt_key(&intent)?;
            if !EffectReceiptRepository::contains(
                tx,
                recovery.message_key,
                receipt.kind,
                &receipt.semantic_identity_hash,
            )
            .await?
            {
                pending.push(intent);
            }
        }
    }
    Ok(pending)
}

pub(crate) fn recovery_intent(
    recovery: &GroupchatNotificationRecovery,
    action: GroupchatNotificationRecoveryAction,
) -> IngressEffectIntent {
    IngressEffectIntent::GroupchatNotificationRecovery {
        mutation: waddle_xmpp::ingress::GroupchatNotificationRecoveryMutation {
            recipient: recovery.key.recipient.clone(),
            room: recovery.key.room.clone(),
            thread_id: recovery.key.thread_id.as_ref().map(|thread| {
                waddle_xmpp_core::mam::ThreadId::new(thread.clone())
                    .expect("validated recovery thread")
            }),
            archive_stanza_id: recovery.key.archive_stanza_id.clone(),
            sender: recovery.sender_jid.clone(),
            is_live_occupant: recovery.is_live_occupant,
            room_members_only: recovery.room_members_only,
            sender_can_broadcast_channel_mention: recovery.sender_can_broadcast_channel_mention,
            created_at_ms: recovery.created_at_ms,
            action,
        },
    }
}
