//! Incremental detached replay writes. Queue history is never reparsed or rewritten.

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use crate::ownership::{Entity, EntityType};
use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::{
    KeyedSnapshotOutcome, PersistedDetachedAppend, PersistedUnackedStanza, SmClaimFence,
};
use crate::Stanza;

use super::{DetachedSession, InMemorySmSessionRegistry, SmRegistryError};

/// Describe the one new entry and exact evictions, including an explicitly
/// sequenced entry that is itself too old to fit the bounded replay window.
pub(super) fn prepare_detached_append(
    previous_sequences: &[u32],
    updated: &DetachedSession,
    sequence: u32,
    stanza: &Stanza,
    original_receipt_at: DateTime<Utc>,
) -> Result<PersistedDetachedAppend, SmRegistryError> {
    let retained: HashSet<_> = updated
        .unacked_stanzas
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    let mut evicted_sequences: Vec<_> = previous_sequences
        .iter()
        .copied()
        .filter(|sequence| !retained.contains(sequence))
        .collect();
    if !retained.contains(&sequence) {
        evicted_sequences.push(sequence);
    }
    Ok(PersistedDetachedAppend {
        session: super::persistence_codec::detached_to_persisted(updated)?,
        stanza: PersistedUnackedStanza {
            ingress_receipts: Vec::new(),
            stream_id: SmSessionId::new(&updated.stream_id),
            sequence,
            stanza: Box::new(stanza.clone()),
            original_receipt_at,
        },
        evicted_sequences,
        ingress_append: None,
        new_delivery: false,
    })
}

impl InMemorySmSessionRegistry {
    /// Recover only an unrecorded first fence. Existing captures never adopt
    /// a replacement epoch. Callers retain this stream's operation guard.
    pub(super) async fn detached_append_fence_locked(
        &self,
        session: &DetachedSession,
    ) -> Option<SmClaimFence> {
        if let Some(fence) = session.promotion_custody.captured_fence() {
            return Some(fence.clone());
        }
        if !matches!(
            session.promotion_custody,
            super::PromotionCaptureCustody::NeverClaimed(_)
        ) {
            return None;
        }
        self.validate_promotion_capture_locked(session).ok()?;
        let identity = self.node_identity.current();
        if !identity.is_active()
            || session
                .promotion_custody
                .first_claim_owner()
                .is_some_and(|owner| owner != &identity)
        {
            return None;
        }
        // The delta may only extend a canonical source that still exists.
        // This also prevents fresh admission after a successor consumed it.
        self.persistence
            .as_ref()?
            .get_session(&SmSessionId::new(&session.stream_id))
            .await
            .ok()??;
        if self.node_identity.current() != identity {
            return None;
        }
        if session.promotion_custody.first_claim_owner().is_none() {
            // Cancellation may precede registry acquire-on-detach. The fenced
            // snapshot already owns its durable claim; append may only adopt
            // that existing authority, never initiate a replacement claim.
            let entity = Entity::new(EntityType::SmSession, &session.stream_id);
            let claim = tokio::time::timeout(
                super::core::CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                self.claim_store.current_claim(&entity),
            )
            .await
            .ok()?
            .ok()??;
            if claim.owner != identity || self.node_identity.current() != identity {
                return None;
            }
            // Pin only after observing ownership. Portable sources with no
            // claim must retain their legitimate first-admission opportunity.
            session.promotion_custody.begin_first_claim(&identity);
        }
        if let Some(owner) = session.promotion_custody.first_claim_owner() {
            // A timed-out detach retains its operation-owned reservation.
            // Resolve it read-only before attempting any new admission.
            self.reconcile_uncertain_claim_acquisition_locked(
                &session.stream_id,
                owner.clone(),
                super::core::PendingClaimAcquisitionDisposition::RetainDetachedSession,
            )
            .await;
        }
        if self.node_identity.current() != identity
            || self.has_claim_fence_reservation(&session.stream_id)
        {
            return None;
        }
        // Reuse the existing capture/identity/capacity checks. Portable stores
        // retain their historical ability to write without a claim; fenced
        // persistence rejects None if recovery is still unavailable.
        match self.promotion_capture_fence_locked(session).await {
            Ok(fence) => Some(fence),
            Err(error) => {
                tracing::debug!(stream_id = %session.stream_id, %error,
                    "detached append could not recover its initial ownership fence");
                None
            }
        }
    }

    pub(super) async fn append_detached_stanza(
        &self,
        stream_id: &str,
        predicate: impl Fn(&DetachedSession) -> bool,
        sequence: Option<u32>,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        let lock = self.stream_lock(stream_id)?;
        let guard = lock.lock_owned().await;
        let session_id = SmSessionId::new(stream_id);
        self.reconcile_stale_session_locked(&session_id).await?;
        let Some(previous) = self.detached_snapshot_matching(&session_id, predicate)? else {
            return Ok(false);
        };
        let previous_sequences: Vec<_> = previous
            .unacked_stanzas
            .iter()
            .map(|entry| entry.sequence)
            .collect();
        let mut updated = previous;
        let sequence = match sequence {
            Some(sequence) => {
                if !updated.record_detached_outbound_at(sequence, stanza, original_receipt_at) {
                    return Ok(true);
                }
                sequence
            }
            None => {
                updated.record_detached_outbound(stanza, original_receipt_at);
                updated.outbound_count
            }
        };
        // The operation guard covers ownership recovery and storage. Moving it
        // into the storage task keeps cancellation from exposing an unresolved
        // commit to the next writer; the stale mark makes that writer reload.
        let _guard = if let Some(storage) = &self.persistence {
            let append = prepare_detached_append(
                &previous_sequences,
                &updated,
                sequence,
                stanza,
                original_receipt_at,
            )?;
            let fence = self.detached_append_fence_locked(&updated).await;
            self.mark_snapshot_stale(&session_id)?;
            let storage = std::sync::Arc::clone(storage);
            let (outcome, guard) = tokio::spawn(async move {
                let outcome = storage
                    .append_detached_stanza_atomic(append, fence.as_ref())
                    .await;
                (outcome, guard)
            })
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))?;
            match outcome.map_err(|error| SmRegistryError::Internal(error.to_string()))? {
                KeyedSnapshotOutcome::Committed => guard,
                _ => {
                    return Err(SmRegistryError::Internal(
                        "unkeyed detached append returned a ledger conflict".into(),
                    ))
                }
            }
        } else {
            guard
        };
        self.publish_detached_snapshot(&session_id, updated)
    }
}
