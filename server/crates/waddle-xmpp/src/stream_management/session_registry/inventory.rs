//! Durable unavailable-resource custody, using expired SM rows as inventory.
use crate::stream_management::persistence::SmClaimFence;
use crate::stream_management::persistence::SmPersistenceError;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::{PersistedUnackedStanza, PromotionAppendOutcome};
use crate::Stanza;

use super::claims::PendingPromotionRetryLease;
use super::persistence_codec::{
    detached_to_persisted, parse_xml_to_persisted_unacked, persisted_to_detached,
};
use super::{DetachedSession, InMemorySmSessionRegistry, ShutdownSmClaimLoss, SmRegistryError};

fn lock_error<T>(_: std::sync::PoisonError<T>) -> SmRegistryError {
    SmRegistryError::Internal("Promotion inventory lock poisoned".to_owned())
}

impl InMemorySmSessionRegistry {
    pub(super) fn promotion_source_locked(
        &self,
        stream_id: &str,
        predicate: impl Fn(&DetachedSession) -> bool,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        Ok(self
            .sessions
            .read()
            .map_err(lock_error)?
            .get(stream_id)
            .filter(|session| predicate(session))
            .cloned())
    }

    /// Durably lease a queue for unavailable-resource handling. The returned
    /// snapshot is authoritative: repeated registration never resurrects rows
    /// already transferred or scrubbed. This is not a resumable session.
    pub async fn register_promotion_session(
        &self,
        session: &DetachedSession,
    ) -> Result<DetachedSession, SmRegistryError> {
        // Synthetic terminal queues have no resumable-map entry. Publish
        // custody before backend awaits so claim maintenance cannot mistake
        // their in-flight durable registration for an abandoned lifecycle.
        {
            // Inventory-before-fence is also the demotion lock order. Keep
            // validation and publication in this same synchronous section.
            let mut pending = self.pending_promotions.write().map_err(lock_error)?;
            let fences = self.claim_fences.read().map_err(lock_error)?;
            self.validate_promotion_capture_with_fences(session, &fences)?;
            #[cfg(test)]
            super::tests::promotion_custody::pause_capture_publication(&session.stream_id);
            let identity = (session.jid.clone(), session.occupancy_session);
            if pending
                .get(&session.stream_id)
                .is_some_and(|existing| *existing != identity)
            {
                return Err(SmRegistryError::Internal(
                    "Promotion inventory belongs to a different lifecycle".to_owned(),
                ));
            }
            pending.insert(session.stream_id.clone(), identity);
        }
        let mut retry = PendingPromotionRetryLease::new(self, session.clone());
        let stream_lock = self.stream_lock(&session.stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        if let Err(error) = self
            .reconcile_promotion_session_locked(retry.session_mut())
            .await
        {
            retry.discard();
            return Err(error);
        }
        self.sessions
            .write()
            .map_err(lock_error)?
            .remove(&session.stream_id);
        self.claimed_sessions
            .write()
            .map_err(lock_error)?
            .remove(&session.stream_id);
        Ok(retry.finish())
    }

    pub(super) async fn persist_promotion_inventory_locked(
        &self,
        session: &DetachedSession,
    ) -> Result<DetachedSession, SmRegistryError> {
        self.persist_promotion_inventory_with_source_locked(session, false)
            .await
    }

    /// Reconcile an existing registered source, including accepted suffixes
    /// whose earlier append failed. The source itself must never be recreated.
    pub(super) async fn refresh_promotion_inventory_locked(
        &self,
        session: &DetachedSession,
    ) -> Result<DetachedSession, SmRegistryError> {
        self.persist_promotion_inventory_with_source_locked(session, true)
            .await
    }

    async fn persist_promotion_inventory_with_source_locked(
        &self,
        session: &DetachedSession,
        require_existing: bool,
    ) -> Result<DetachedSession, SmRegistryError> {
        let fence = if require_existing {
            self.registered_promotion_fence_locked(session).await?
        } else {
            self.promotion_capture_fence_locked(session).await?
        };
        let stream = SmSessionId::new(&session.stream_id);
        let mut inventory = session.clone();
        inventory.max_resume_time = Some(0);
        if let Some(storage) = &self.persistence {
            if session.promotion_custody.requires_existing_source() {
                // last_acked retains the expiry completion's ACK intent across
                // cancellation. Apply it atomically before loading the queue;
                // otherwise a failed conversion could reload acknowledged rows.
                storage
                    .mark_session_nonresumable(&stream, session.last_acked, &fence, None)
                    .await
                    .map_err(|error| storage_error(error, &fence))?;
            }
            if let Some(mut persisted) = storage
                .get_session(&stream)
                .await
                .map_err(|error| storage_error(error, &fence))?
            {
                let resumable = persisted.max_resume_time != Some(0)
                    || persisted.max_resume_duration != std::time::Duration::ZERO;
                if require_existing && resumable {
                    return Err(SmRegistryError::Internal(
                        "Promotion refresh requires non-resumable inventory".to_owned(),
                    ));
                }
                persisted.max_resume_time = Some(0);
                persisted.max_resume_duration = std::time::Duration::ZERO;
                if resumable {
                    self.mark_snapshot_stale(&stream)?;
                    // Metadata-only update preserves concurrent sink pruning.
                    storage
                        .upsert_promotion_session(persisted.clone(), &fence)
                        .await
                        .map_err(|error| storage_error(error, &fence))?;
                }
                // A terminal capture can extend the last durable snapshot.
                // Only genuinely newer sequences may be merged: an absent
                // row at or below the watermark was already settled/scrubbed.
                let watermark = persisted.outbound_count;
                let mut suffix = session
                    .unacked_stanzas
                    .iter()
                    .filter(|entry| super::super::sequence::sequence_gt(entry.sequence, watermark))
                    .collect::<Vec<_>>();
                suffix.sort_by_key(|entry| entry.sequence.wrapping_sub(watermark));
                for entry in suffix {
                    let stanza = parse_xml_to_persisted_unacked(
                        &session.stream_id,
                        entry.sequence,
                        &entry.stanza_xml,
                        entry.original_receipt_at,
                        entry.ingress_receipts.clone(),
                    )?;
                    storage
                        .append_promotion_stanza_with_fence(stanza, &fence)
                        .await
                        .map_err(|error| storage_error(error, &fence))?;
                }
                persisted = storage
                    .get_session(&stream)
                    .await
                    .map_err(|error| storage_error(error, &fence))?
                    .ok_or_else(|| {
                        SmRegistryError::Internal("Promotion inventory disappeared".to_owned())
                    })?;
                let rows = storage
                    .list_unacked(&stream)
                    .await
                    .map_err(|error| storage_error(error, &fence))?;
                inventory = persisted_to_detached(&persisted, &rows)?;
                inventory.promotion_custody = session.promotion_custody.clone();
                inventory.detached_at = session.detached_at;
                inventory.pending_subscribes_flushed = session.pending_subscribes_flushed;
            } else {
                // Expiry-conversion retries may still find resumable metadata
                // after cancellation, but unlike fresh terminal admissions
                // they must never reconstruct a vanished canonical source.
                if require_existing || session.promotion_custody.requires_existing_source() {
                    return Err(SmRegistryError::Internal(
                        "Registered promotion inventory disappeared".to_owned(),
                    ));
                }
                let rows = inventory
                    .unacked_stanzas
                    .iter()
                    .map(|entry| {
                        parse_xml_to_persisted_unacked(
                            &inventory.stream_id,
                            entry.sequence,
                            &entry.stanza_xml,
                            entry.original_receipt_at,
                            entry.ingress_receipts.clone(),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                // There is no previously published durable snapshot to
                // protect here. Keeping a stale mark after a failed initial
                // write would make drain reject the missing row forever,
                // before it could retry persisting this retained capture.
                storage
                    .store_promotion_session_atomic(
                        detached_to_persisted(&inventory)?,
                        rows,
                        &fence,
                    )
                    .await
                    .map_err(|error| storage_error(error, &fence))?;
            }
        }
        Ok(inventory)
    }

    /// Validate a capture against its own authority, never a replacement
    /// incarnation found through the mutable stream-id cache.
    pub(super) fn validate_promotion_capture_locked(
        &self,
        session: &DetachedSession,
    ) -> Result<(), SmRegistryError> {
        let fences = self.claim_fences.read().map_err(lock_error)?;
        self.validate_promotion_capture_with_fences(session, &fences)
    }

    pub(super) fn validate_promotion_capture_with_fences(
        &self,
        session: &DetachedSession,
        fences: &HashMap<String, SmClaimFence>,
    ) -> Result<(), SmRegistryError> {
        if let Some(expected) = session.promotion_custody.fence() {
            if fences.get(&session.stream_id) != Some(expected) {
                return Err(SmRegistryError::PromotionClaimLost(
                    ShutdownSmClaimLoss::Recorded(expected.clone()),
                ));
            }
        } else if matches!(
            session.promotion_custody,
            super::PromotionCaptureCustody::Acquiring(_)
        ) {
            let pending = self
                .pending_resume_claim_custody
                .read()
                .map_err(lock_error)?;
            if !pending
                .get(&session.stream_id)
                .is_some_and(|proof| proof.same_acquisition(&session.promotion_custody))
            {
                return Err(SmRegistryError::StorageUnavailable(
                    super::traits::StorageOutageCause::Backend,
                ));
            }
        } else if matches!(
            session.promotion_custody,
            super::PromotionCaptureCustody::Unproven
        ) {
            return Err(SmRegistryError::StorageUnavailable(
                super::traits::StorageOutageCause::Backend,
            ));
        }
        Ok(())
    }

    pub(super) async fn registered_promotion_fence_locked(
        &self,
        session: &DetachedSession,
    ) -> Result<SmClaimFence, SmRegistryError> {
        if session.promotion_custody.fence().is_none() {
            return Err(SmRegistryError::StorageUnavailable(
                super::traits::StorageOutageCause::Backend,
            ));
        }
        self.promotion_capture_fence_locked(session).await
    }

    pub(super) async fn promotion_capture_fence_locked(
        &self,
        session: &DetachedSession,
    ) -> Result<SmClaimFence, SmRegistryError> {
        self.validate_promotion_capture_locked(session)?;
        if session.promotion_custody.fence().is_none() {
            if matches!(
                session.promotion_custody,
                super::PromotionCaptureCustody::Acquiring(_)
            ) {
                return Err(SmRegistryError::StorageUnavailable(
                    super::traits::StorageOutageCause::Backend,
                ));
            }
            // Only explicit fresh admission may acquire. Bind the first
            // result into the shared capture before any subsequent await.
            if let Some(fence) = self
                .current_sm_claim_fence(&session.stream_id)
                .filter(|_| session.promotion_custody.first_claim_owner().is_none())
            {
                session.promotion_custody.bind_first_claim(fence);
            } else {
                let identity = self.node_identity.current();
                if !identity.is_active() {
                    return Err(SmRegistryError::PromotionClaimLost(
                        ShutdownSmClaimLoss::Disabled(identity),
                    ));
                }
                let Some(authority) = self
                    .ensure_session_claim_for_capture(
                        &session.stream_id,
                        &session.promotion_custody,
                    )
                    .await
                else {
                    return Err(SmRegistryError::StorageUnavailable(
                        super::traits::StorageOutageCause::Backend,
                    ));
                };
                drop(authority);
            }
        }
        let fence = session.promotion_custody.fence().cloned().ok_or_else(|| {
            SmRegistryError::Internal("Promotion capture has no acquired fence".to_owned())
        })?;
        self.validate_promotion_capture_locked(session)?;
        let Some(authority) = self
            .retirement_identity_locked(&SmSessionId::new(&session.stream_id))
            .await?
        else {
            return Err(SmRegistryError::PromotionClaimLost(
                ShutdownSmClaimLoss::Recorded(fence),
            ));
        };
        drop(authority);
        Ok(fence)
    }

    /// Extend registered non-resumable inventory, atomically advancing its
    /// watermark without replacing rows a concurrent sink may have pruned.
    pub async fn append_promotion_stanza(
        &self,
        capture: &DetachedSession,
        stanza: &Stanza,
        sequence: u32,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<PromotionAppendOutcome, SmRegistryError> {
        let stream_id = &capture.stream_id;
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        if capture.promotion_custody.fence().is_none() {
            return Err(SmRegistryError::StorageUnavailable(
                super::traits::StorageOutageCause::Backend,
            ));
        }
        self.validate_promotion_capture_locked(capture)?;
        let stream = SmSessionId::new(stream_id);
        let fence =
            self.current_sm_claim_fence(stream_id)
                .ok_or(SmRegistryError::StorageUnavailable(
                    super::traits::StorageOutageCause::Backend,
                ))?;
        let Some(authority) = self.retirement_identity_locked(&stream).await? else {
            return Err(SmRegistryError::PromotionClaimLost(
                ShutdownSmClaimLoss::Recorded(fence),
            ));
        };
        drop(authority);
        if !self
            .pending_promotions
            .read()
            .map_err(lock_error)?
            .contains_key(stream_id.as_str())
        {
            return Err(SmRegistryError::Internal(
                "Promotion inventory is not registered".to_owned(),
            ));
        }
        if let Some(storage) = &self.persistence {
            return storage
                .append_promotion_stanza_with_fence(
                    PersistedUnackedStanza {
                        stream_id: stream,
                        sequence,
                        stanza: Box::new(stanza.clone()),
                        original_receipt_at,
                        ingress_receipts: Vec::new(),
                    },
                    &fence,
                )
                .await
                .map_err(|error| storage_error(error, &fence));
        }
        Ok(PromotionAppendOutcome::Appended)
    }
}

fn storage_error(error: SmPersistenceError, fence: &SmClaimFence) -> SmRegistryError {
    match error {
        crate::stream_management::persistence::SmPersistenceError::NotOwner { .. } => {
            SmRegistryError::PromotionClaimLost(ShutdownSmClaimLoss::Recorded(fence.clone()))
        }
        other => SmRegistryError::Internal(other.to_string()),
    }
}
