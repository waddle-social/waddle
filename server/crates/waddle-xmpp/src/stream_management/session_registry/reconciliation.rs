//! Snapshot publication and cancellation recovery. Every async helper here is
//! called with the stream shard already held; none acquires a shard or identity.
use std::collections::HashSet;

use crate::pending_delivery::SmSessionId;

use super::persistence_codec::persisted_to_detached;
use super::{DetachedSession, InMemorySmSessionRegistry, SmRegistryError};

pub(super) struct PromotionSnapshot {
    outbound_count: u32,
    sequences: HashSet<u32>,
}

impl PromotionSnapshot {
    fn from_session(session: &DetachedSession) -> Self {
        Self {
            outbound_count: session.outbound_count,
            sequences: session
                .unacked_stanzas
                .iter()
                .map(|entry| entry.sequence)
                .collect(),
        }
    }

    fn covers(&self, session: &DetachedSession) -> bool {
        self.outbound_count == session.outbound_count
            && session
                .unacked_stanzas
                .iter()
                .all(|entry| self.sequences.contains(&entry.sequence))
    }
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> SmRegistryError {
    SmRegistryError::Internal("Snapshot reconciliation lock poisoned".to_owned())
}

impl InMemorySmSessionRegistry {
    pub(super) fn mark_snapshot_stale(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<(), SmRegistryError> {
        if self.persistence.is_some() {
            self.stale_snapshots
                .write()
                .map_err(lock_error)?
                .insert(stream_id.clone());
        }
        Ok(())
    }

    fn clear_snapshot_stale(&self, stream_id: &SmSessionId) -> Result<(), SmRegistryError> {
        self.stale_snapshots
            .write()
            .map_err(lock_error)?
            .remove(stream_id);
        Ok(())
    }

    pub(super) fn detached_snapshot_matching(
        &self,
        stream_id: &SmSessionId,
        predicate: impl Fn(&DetachedSession) -> bool,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let current = self
            .sessions
            .read()
            .map_err(lock_error)?
            .get(stream_id.as_str())
            .filter(|session| predicate(session))
            .cloned();
        if current.is_some() {
            return Ok(current);
        }
        Ok(self
            .claimed_sessions
            .read()
            .map_err(lock_error)?
            .get(stream_id.as_str())
            .filter(|session| predicate(session))
            .cloned())
    }

    /// `false` means committed but displaced, not a failed allocation. Leave
    /// the stale mark set so the captured promotion copy must be reconciled.
    pub(super) fn publish_detached_snapshot(
        &self,
        stream_id: &SmSessionId,
        updated: DetachedSession,
    ) -> Result<bool, SmRegistryError> {
        {
            let mut sessions = self.sessions.write().map_err(lock_error)?;
            if let Some(session) = sessions.get_mut(stream_id.as_str()) {
                *session = updated;
                self.clear_snapshot_stale(stream_id)?;
                return Ok(true);
            }
        }
        {
            let mut claimed = self.claimed_sessions.write().map_err(lock_error)?;
            if let Some(session) = claimed.get_mut(stream_id.as_str()) {
                *session = updated;
                self.clear_snapshot_stale(stream_id)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn durable_snapshot_locked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(None);
        };
        let persisted = storage
            .get_session(stream_id)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))?;
        let Some(persisted) = persisted else {
            return Ok(None);
        };
        let unacked = storage
            .list_unacked(stream_id)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))?;
        let mut session = persisted_to_detached(&persisted, &unacked)?;
        if let Some(fence) = self.current_sm_claim_fence(stream_id.as_str()) {
            session.promotion_custody = super::PromotionCaptureCustody::claimed(fence);
        }
        Ok(Some(session))
    }

    /// A cancelled write leaves this mark behind. Read durable state before
    /// any subsequent mutation or consumption, never overwrite it with Q.
    pub(super) async fn reconcile_stale_session_locked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<(), SmRegistryError> {
        if !self
            .stale_snapshots
            .read()
            .map_err(lock_error)?
            .contains(stream_id)
        {
            return Ok(());
        }
        let Some(mut durable) = self.durable_snapshot_locked(stream_id).await? else {
            // An uncertain deletion or ownership transition cannot justify
            // handing out the old replay queue. Keep the mark and fail closed.
            return Err(SmRegistryError::Internal(
                "Stale SM snapshot has no durable session".to_owned(),
            ));
        };
        if let Some(current) = self.detached_snapshot_matching(stream_id, |_| true)? {
            durable.detached_at = current.detached_at;
            durable.pending_subscribes_flushed = current.pending_subscribes_flushed;
            durable.promotion_custody = current.promotion_custody;
        }
        self.publish_detached_snapshot(stream_id, durable)?;
        Ok(())
    }

    /// Refresh a captured off-map queue after all earlier appends on this
    /// shard finish. The registered baseline is what confirm may retire.
    pub(super) async fn reconcile_promotion_session_locked(
        &self,
        session: &mut DetachedSession,
    ) -> Result<(), SmRegistryError> {
        *session = self.persist_promotion_inventory_locked(session).await?;
        self.record_promotion_snapshot_locked(session)
    }

    /// Record the authoritative snapshot returned by registration while the
    /// caller still holds the same stream shard. No second registration or
    /// backend read is needed before handing that capture to its owner.
    pub(super) fn record_promotion_snapshot_locked(
        &self,
        session: &DetachedSession,
    ) -> Result<(), SmRegistryError> {
        let stream_id = SmSessionId::new(&session.stream_id);
        if self.persistence.is_some() {
            self.promotion_snapshots
                .write()
                .map_err(lock_error)?
                .insert(stream_id.clone(), PromotionSnapshot::from_session(session));
            self.clear_snapshot_stale(&stream_id)?;
        }
        Ok(())
    }

    /// Refresh inventory that was already registered, without admitting a
    /// source, reacquiring a claim, or recreating a missing canonical row.
    pub async fn refresh_promotion_session(
        &self,
        session: &DetachedSession,
    ) -> Result<DetachedSession, SmRegistryError> {
        let lock = self.stream_lock(&session.stream_id)?;
        let _guard = lock.lock().await;
        // Return a replacement capture only after every accepted newer suffix
        // has durable custody; an error leaves the caller's original intact.
        let durable = self.refresh_promotion_inventory_locked(session).await?;
        self.record_promotion_snapshot_locked(&durable)?;
        Ok(durable)
    }

    /// A caller confirms only a stream id. Check it against the queue handed
    /// out for promotion, allowing durable row removals but no unseen append.
    /// On mismatch retain the new queue for the normal promotion retry path.
    pub(super) async fn confirm_promotion_snapshot_locked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<bool, SmRegistryError> {
        if self.persistence.is_none() {
            return Ok(true);
        }
        let Some(durable) = self.durable_snapshot_locked(stream_id).await? else {
            return Ok(true);
        };
        let covered = self
            .promotion_snapshots
            .read()
            .map_err(lock_error)?
            .get(stream_id)
            .is_some_and(|snapshot| snapshot.covers(&durable));
        if !covered {
            self.pending_promotion_retries
                .write()
                .map_err(lock_error)?
                .insert(stream_id.as_str().to_owned(), durable);
        }
        Ok(covered)
    }

    pub(super) fn forget_snapshot_reconciliation(&self, stream_id: &SmSessionId) {
        if let Ok(mut snapshots) = self.promotion_snapshots.write() {
            snapshots.remove(stream_id);
        }
        if let Ok(mut stale) = self.stale_snapshots.write() {
            stale.remove(stream_id);
        }
    }
}
