//! Replace a timed-out direct-send obligation with durable offline custody.
//!
//! A lost socket acknowledgement can make this duplicate a delivered copy. The
//! transaction records real pending custody, never a successful socket send.
use jid::FullJid;
use sha2::{Digest, Sha256};
use waddle_xmpp::{
    ingress::{IngressEffectIntent, MessageKey},
    pending_delivery::{InsertOutcome, PendingPayload, PendingRow, PendingRowId},
    protocol::{
        dm_routing::{classify_dm_intake, OnlineResources, PendingDecision},
        Blocklist,
    },
};

use crate::{
    ingress::{recorded::RouteProgress, recovery_rebuild},
    ingress_uow::{
        settle_recorded, CanonicalMessageRepository, DeliveryProgressRepository,
        EffectIntentRepository, EffectReceiptRepository, IngressUnitOfWork, IngressUowError,
        PendingReceiptRepository, SendAttemptRepository, SendAttemptStatus, SendObligation,
    },
    server::routes::interpret::Deps,
};

#[derive(Debug, thiserror::Error)]
pub(super) enum HandoffError {
    #[error(transparent)]
    Ingress(#[from] IngressUowError),
    #[error(transparent)]
    Pending(#[from] waddle_xmpp::pending_delivery::storage::PendingStorageError),
}

/// `Some` proves the recipient-wide route was settled. The aggregate receipt
/// survives pending-row consumption and prevents a sibling from inserting a
/// second bare-JID offline copy. It does not assert a successful socket send.
pub(super) async fn handoff(
    uow: &IngressUnitOfWork,
    deps: &Deps<'_>,
    key: MessageKey,
    progress: &RouteProgress,
    resource: &FullJid,
) -> Result<Option<Vec<IngressEffectIntent>>, HandoffError> {
    let Some(storage) = deps.pending_delivery_storage else {
        return Ok(None);
    };
    let recipient = resource.to_bare();
    // Policy I/O must precede the canonical write lock. A storage failure is
    // uncertainty, never permission to bypass the recipient's blocklist.
    let blocklist = match deps.blocking_storage {
        Some(storage) => Blocklist::new(
            storage
                .list_blocked_jid_entries(&recipient)
                .await
                .map_err(IngressUowError::BlocklistUnavailable)?,
        ),
        None => Blocklist::default(),
    };
    let mut tx = uow
        .begin_with_timeouts(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(250),
        )
        .await?;
    let intents = EffectIntentRepository::load(&mut tx, key).await?;
    // Fanout is a semantic set. Canonicalize the offered typed evidence using
    // the same durable codec before comparing it to the recorded authority.
    let intent = progress
        .settle_evidence()
        .with_encoded_v1(IngressEffectIntent::decode_v1)
        .map_err(IngressUowError::from)?
        .map_err(IngressUowError::from)?;
    if crate::ingress::receipt_key(&intent)? != progress.receipt {
        return Err(IngressUowError::EffectIntentConflict.into());
    }
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await?
        .ok_or(IngressUowError::EffectIntentMessageMissing)?;
    if !intents.contains(&intent)
        || !progress.fanout.contains(resource)
        || !recovery_rebuild::rebuildable_direct_route(&envelope, &intents, &intent)
    {
        return Ok(None);
    }
    if EffectReceiptRepository::contains(
        &mut tx,
        key,
        progress.receipt.kind,
        &progress.receipt.semantic_identity_hash,
    )
    .await?
    {
        // A policy discard or another executor may settle the whole route
        // without recording per-resource delivery. Its receipt is authoritative.
        tx.commit().await?;
        return Ok(Some(vec![intent]));
    }
    let obligation = SendObligation {
        message: key,
        receipt: progress.receipt.clone(),
        recipient: resource.clone(),
    };
    if !matches!(
        SendAttemptRepository::status(&mut tx, &obligation).await?,
        Some(SendAttemptStatus::ExpiredStarted | SendAttemptStatus::ExpiredLease)
    ) || SendAttemptRepository::has_custody(&mut tx, &obligation).await?
    {
        return Ok(None);
    }
    if DeliveryProgressRepository::load(&mut tx, key, &progress.receipt)
        .await?
        .contains(resource)
        || SendAttemptRepository::has_resource_receipt(&mut tx, &obligation).await?
    {
        return Ok(None);
    }
    // Offline storage is recipient-wide. Do not hand off while any sibling
    // holds active sink authority, even if this resource's own lease expired.
    // The canonical lock protects this entire scan and the final settlement.
    for sibling in &progress.fanout {
        let sibling_obligation = SendObligation {
            recipient: sibling.clone(),
            ..obligation.clone()
        };
        if matches!(
            SendAttemptRepository::status(&mut tx, &sibling_obligation).await?,
            Some(SendAttemptStatus::Leased | SendAttemptStatus::Started)
        ) {
            return Ok(None);
        }
    }
    let routing = classify_dm_intake(envelope.message(), &OnlineResources::empty(), &blocklist);
    let payload = match routing.pending {
        PendingDecision::Archived => {
            let Some(stanza_id) = intents.iter().find_map(|intent| match intent {
                IngressEffectIntent::ArchiveAuthoritative {
                    archive, stanza_id, ..
                } if archive == &recipient => Some(stanza_id.clone()),
                _ => None,
            }) else {
                return Ok(None);
            };
            Some(PendingPayload::Archived(stanza_id))
        }
        PendingDecision::Transient => Some(PendingPayload::Transient(Box::new(
            crate::ingress::recorded::delivery_message(&envelope, &recipient, &intents),
        ))),
        // The normal offline classifier forbids storage (including a
        // newly blocked sender or no-store). Resolve by that policy; do
        // not retain the message indefinitely or fabricate an enqueue.
        PendingDecision::None => None,
    };
    if let Some(payload) = payload {
        let row = PendingRow {
            id: pending_id(key, &progress.receipt),
            recipient,
            original_receipt_at: CanonicalMessageRepository::created_at(&mut tx, key).await?,
            payload,
            flushed_in_session: None,
            outbound_sequence: None,
        };
        let existing_custody = match &row.payload {
            PendingPayload::Archived(stanza_id) => {
                PendingReceiptRepository::has_archived_custody(&mut tx, &row.recipient, stanza_id)
                    .await?
            }
            PendingPayload::Transient(_) => false,
        };
        if !existing_custody
            && PendingReceiptRepository::insert(&mut tx, &row, storage.quota_policy()).await?
                == InsertOutcome::QuotaExceeded
        {
            // Preserve authority for retry; there is no durable custody.
            return Ok(None);
        }
        // Archived rows remain unoutboxed. The existing pending-delivery
        // notification janitor applies normal push policy and retries.
    }
    for sibling in &progress.fanout {
        SendAttemptRepository::retire_expired_attempt(
            &mut tx,
            &SendObligation {
                recipient: sibling.clone(),
                ..obligation.clone()
            },
        )
        .await?;
    }
    let settled = settle_recorded(&mut tx, key, &[intent]).await?;
    tx.commit().await?;
    Ok(Some(settled))
}

fn pending_id(
    key: MessageKey,
    receipt: &crate::ingress::decision::EffectReceiptKey,
) -> PendingRowId {
    let mut hash = Sha256::new();
    hash.update(b"waddle-ingress-ambiguous-offline-v1");
    hash.update(key.to_storage().as_bytes());
    hash.update(receipt.kind.to_storage().to_be_bytes());
    hash.update(receipt.semantic_identity_hash);
    PendingRowId::new(hex::encode(hash.finalize()))
}

#[cfg(test)]
#[path = "execute_ambiguous_offline_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "execute_ambiguous_notification_tests.rs"]
mod notification_tests;
