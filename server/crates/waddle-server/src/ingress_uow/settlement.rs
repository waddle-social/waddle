//! Match durable evidence to the exact obligations retained by ingress.
use waddle_xmpp::ingress::{
    GroupchatNotificationRecoveryAction, IngressEffectIntent, MessageKey,
    NotificationActivityMutation, NotificationCandidateOutcome,
};

use super::{
    EffectIntentRepository, EffectReceiptRepository, IngressUowError, IngressUowTransaction,
};

#[cfg(test)]
tokio::task_local! {
    pub(super) static POOLED_RECEIPT_WRITES: std::cell::Cell<u64>;
}

/// Settle only recorded obligations. The caller must hold
/// [`super::CanonicalMessageRepository::lock`] before writing effect tables or
/// calling this function, following epoch → canonical → effects → receipts.
/// Returned intents are the recorded values, whose receipts become durable
/// only when the caller commits this transaction.
pub(crate) async fn settle_recorded(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    evidence: &[IngressEffectIntent],
) -> Result<Vec<IngressEffectIntent>, IngressUowError> {
    let recorded = EffectIntentRepository::load(tx, key).await?;
    let mut settled = Vec::new();
    for intent in recorded {
        if !evidence.iter().any(|proof| discharges(&intent, proof)) {
            continue;
        }
        let receipt = crate::ingress::receipt_key(&intent)?;
        EffectReceiptRepository::record_receipt(
            tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await?;
        settled.push(intent);
    }
    Ok(settled)
}

/// A duplicate candidate proves the same durable candidate as its insertion.
/// Completed recovery also discharges a deferred policy decision with identical frozen fields.
fn discharges(recorded: &IngressEffectIntent, evidence: &IngressEffectIntent) -> bool {
    if recorded == evidence {
        return true;
    }
    if let (
        IngressEffectIntent::GroupchatNotificationRecovery { mutation: recorded },
        IngressEffectIntent::GroupchatNotificationRecovery { mutation: evidence },
    ) = (recorded, evidence)
    {
        if recorded.action == GroupchatNotificationRecoveryAction::DeferredPolicy
            && evidence.action == GroupchatNotificationRecoveryAction::Completed
        {
            let mut completed = recorded.clone();
            completed.action = GroupchatNotificationRecoveryAction::Completed;
            return completed == *evidence;
        }
    }
    matches!(
        (recorded, evidence),
        (
            IngressEffectIntent::NotificationActivityPreview {
                owner,
                mutation: NotificationActivityMutation::NotificationCandidate {
                    conversation,
                    archive_stanza_id,
                    outcome: NotificationCandidateOutcome::Inserted,
                },
            },
            IngressEffectIntent::NotificationActivityPreview {
                owner: proven_owner,
                mutation: NotificationActivityMutation::NotificationCandidate {
                    conversation: proven_conversation,
                    archive_stanza_id: proven_archive,
                    outcome: NotificationCandidateOutcome::Duplicate,
                },
            },
        ) if owner == proven_owner
            && conversation == proven_conversation
            && archive_stanza_id == proven_archive
    )
}

#[cfg(test)]
#[path = "settlement_tests.rs"]
mod tests;
