//! Durable unavailable-resource custody, using expired SM rows as inventory.
use chrono::{DateTime, Utc};

use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::PersistedUnackedStanza;
use crate::Stanza;

use super::claims::PendingPromotionRetryLease;
use super::persistence_codec::{
    detached_to_persisted, parse_xml_to_persisted_unacked, persisted_to_detached,
};
use super::{DetachedSession, InMemorySmSessionRegistry, SmRegistryError};

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
            let mut pending = self.pending_promotions.write().map_err(lock_error)?;
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
        // A recorded fence must still authorize this exact incarnation. Never
        // replace a lost fence by reacquiring a newer claim for stale work.
        if self.current_sm_claim_fence(&session.stream_id).is_some() {
            let Some(authority) = self
                .retirement_identity_locked(&SmSessionId::new(&session.stream_id))
                .await?
            else {
                return Err(SmRegistryError::Internal(
                    "Promotion inventory claim lost".to_owned(),
                ));
            };
            drop(authority);
        } else {
            let Some(authority) = self.ensure_session_claim(&session.stream_id).await else {
                return Err(SmRegistryError::Internal(
                    "Promotion inventory claim unavailable".to_owned(),
                ));
            };
            drop(authority);
        }
        let stream = SmSessionId::new(&session.stream_id);
        let mut inventory = session.clone();
        inventory.max_resume_time = Some(0);
        if let Some(storage) = &self.persistence {
            if let Some(mut persisted) =
                storage.get_session(&stream).await.map_err(storage_error)?
            {
                let resumable = persisted.max_resume_time != Some(0)
                    || persisted.max_resume_duration != std::time::Duration::ZERO;
                persisted.max_resume_time = Some(0);
                persisted.max_resume_duration = std::time::Duration::ZERO;
                if resumable {
                    self.mark_snapshot_stale(&stream)?;
                    // Metadata-only update preserves concurrent sink pruning.
                    storage
                        .upsert_session(persisted.clone())
                        .await
                        .map_err(storage_error)?;
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
                        .append_promotion_stanza(stanza)
                        .await
                        .map_err(storage_error)?;
                }
                persisted = storage
                    .get_session(&stream)
                    .await
                    .map_err(storage_error)?
                    .ok_or_else(|| {
                        SmRegistryError::Internal("Promotion inventory disappeared".to_owned())
                    })?;
                let rows = storage.list_unacked(&stream).await.map_err(storage_error)?;
                inventory = persisted_to_detached(&persisted, &rows)?;
                inventory.detached_at = session.detached_at;
                inventory.pending_subscribes_flushed = session.pending_subscribes_flushed;
            } else {
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
                    .store_session_atomic(detached_to_persisted(&inventory)?, rows)
                    .await
                    .map_err(storage_error)?;
            }
        }
        Ok(inventory)
    }

    /// Extend registered non-resumable inventory, atomically advancing its
    /// watermark without replacing rows a concurrent sink may have pruned.
    pub async fn append_promotion_stanza(
        &self,
        stream_id: &str,
        stanza: &Stanza,
        sequence: u32,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<(), SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        let stream = SmSessionId::new(stream_id);
        let Some(authority) = self.retirement_identity_locked(&stream).await? else {
            return Err(SmRegistryError::Internal(
                "Promotion inventory claim lost".to_owned(),
            ));
        };
        drop(authority);
        if !self
            .pending_promotions
            .read()
            .map_err(lock_error)?
            .contains_key(stream_id)
        {
            return Err(SmRegistryError::Internal(
                "Promotion inventory is not registered".to_owned(),
            ));
        }
        if let Some(storage) = &self.persistence {
            storage
                .append_promotion_stanza(PersistedUnackedStanza {
                    stream_id: stream,
                    sequence,
                    stanza: Box::new(stanza.clone()),
                    original_receipt_at,
                    ingress_receipts: Vec::new(),
                })
                .await
                .map_err(storage_error)?;
        }
        Ok(())
    }
}

fn storage_error(
    error: crate::stream_management::persistence::SmPersistenceError,
) -> SmRegistryError {
    SmRegistryError::Internal(error.to_string())
}
