//! Non-ingress extension dispatch has no canonical row or receipt authority.
use super::{
    bounce_offline_quota, insert_prepared_notification, mark_pending_notification_outboxed,
    NotificationCandidateQueueOutcome, PreparedOfflineNotification,
};
use crate::server::routes::interpret::{effects::EffectOutcome, Deps};
use waddle_xmpp::{
    ingress::{
        IngressEffectIntent, NotificationActivityMutation, NotificationCandidateOutcome,
        PendingDeliveryMutation,
    },
    pending_delivery::{InsertOutcome, PendingPayload, PendingRow},
};

pub(crate) async fn execute_immediate(
    deps: &Deps<'_>,
    row: PendingRow,
    prepared: PreparedOfflineNotification,
    original_message: &xmpp_parsers::message::Message,
) -> EffectOutcome {
    let Some(storage) = deps.pending_delivery_storage else {
        return EffectOutcome::Unavailable;
    };
    match storage.insert(row.clone()).await {
        Ok(InsertOutcome::Inserted) => {}
        Ok(InsertOutcome::QuotaExceeded) => {
            bounce_offline_quota(deps, &row.recipient, original_message).await;
            return EffectOutcome::OfflineDeliveryQuotaExceeded;
        }
        Err(error) => {
            tracing::warn!(%error, recipient = %row.recipient, "immediate offline delivery insert failed");
            return EffectOutcome::Unavailable;
        }
    }
    let mutation = match &row.payload {
        PendingPayload::Archived(archive_stanza_id) => PendingDeliveryMutation::Archived {
            recipient: row.recipient.clone(),
            row_id: row.id.clone(),
            archive_stanza_id: archive_stanza_id.clone(),
        },
        PendingPayload::Transient(_) => PendingDeliveryMutation::Transient {
            recipient: row.recipient.clone(),
            row_id: row.id.clone(),
        },
    };
    let mut confirmed = vec![IngressEffectIntent::PendingDelivery { mutation }];
    if let PendingPayload::Archived(archive_stanza_id) = &row.payload {
        let outcome = insert_prepared_notification(deps.web_socket_state, prepared).await;
        let candidate_outcome = match outcome {
            NotificationCandidateQueueOutcome::Inserted => {
                Some(NotificationCandidateOutcome::Inserted)
            }
            NotificationCandidateQueueOutcome::Duplicate => {
                Some(NotificationCandidateOutcome::Duplicate)
            }
            _ => None,
        };
        if let Some(outcome) = candidate_outcome {
            confirmed.push(IngressEffectIntent::NotificationActivityPreview {
                owner: row.recipient.clone(),
                mutation: NotificationActivityMutation::NotificationCandidate {
                    conversation: row.recipient.clone(),
                    archive_stanza_id: archive_stanza_id.clone(),
                    outcome,
                },
            });
        }
        if outcome != NotificationCandidateQueueOutcome::RetryLater
            && mark_pending_notification_outboxed(storage.as_ref(), &row.id, &row.recipient).await
        {
            confirmed.push(IngressEffectIntent::NotificationActivityPreview {
                owner: row.recipient.clone(),
                mutation: NotificationActivityMutation::OfflineDelivery {
                    conversation: row.recipient.clone(),
                    archive_stanza_id: archive_stanza_id.clone(),
                },
            });
        }
    }
    EffectOutcome::ConfirmedIntents(confirmed)
}
