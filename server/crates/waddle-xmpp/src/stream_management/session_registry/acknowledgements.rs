//! Resume ACK evidence survives both source rollback and independent queue outages.

use crate::pending_delivery::{storage::PendingDeliveryStorage, SmSessionId};
use crate::stream_management::persistence::{SmClaimFence, SmPersistenceError};

use super::{DetachedSession, InMemorySmSessionRegistry, ShutdownSmClaimLoss, SmRegistryError};

fn storage_error(error: SmPersistenceError, fence: &SmClaimFence) -> SmRegistryError {
    match error {
        SmPersistenceError::NotOwner { .. } => {
            SmRegistryError::PromotionClaimLost(ShutdownSmClaimLoss::Recorded(fence.clone()))
        }
        error => SmRegistryError::Internal(error.to_string()),
    }
}

impl InMemorySmSessionRegistry {
    fn acknowledgement_fence_locked(
        &self,
        session: &DetachedSession,
    ) -> Result<SmClaimFence, SmRegistryError> {
        self.validate_promotion_capture_locked(session)?;
        session.promotion_custody.captured_fence().cloned().ok_or(
            SmRegistryError::StorageUnavailable(super::traits::StorageOutageCause::Backend),
        )
    }

    /// Commit the client's valid ACK before retiring any row-backed copies.
    /// An uncertain commit leaves the snapshot stale so release, retry, and
    /// expiry reload the durable prefix instead of restoring acknowledged XML.
    pub async fn acknowledge_claimed_resume(
        &self,
        captured: &DetachedSession,
        h: u32,
    ) -> Result<DetachedSession, SmRegistryError> {
        let stream = SmSessionId::new(&captured.stream_id);
        let lock = self.stream_lock(stream.as_str())?;
        let _guard = lock.lock().await;
        self.reconcile_stale_session_locked(&stream).await?;
        let fence = self.acknowledgement_fence_locked(captured)?;
        let mut session = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_owned()))?
            .get(stream.as_str())
            .filter(|session| {
                session.jid == captured.jid
                    && session.occupancy_session == captured.occupancy_session
                    && session.promotion_custody.captured_fence() == Some(&fence)
            })
            .cloned()
            .ok_or_else(|| {
                SmRegistryError::Internal("Resume ACK source is no longer claimed".to_owned())
            })?;
        if !session.can_resume_from(h) || session.handled_count_exceeds_outbound(h) {
            return Err(SmRegistryError::Internal(
                "Resume ACK is outside the claimed replay window".to_owned(),
            ));
        }
        let authority = self
            .node_identity
            .guard_if_current(fence.owner())
            .await
            .ok_or_else(|| {
                SmRegistryError::PromotionClaimLost(ShutdownSmClaimLoss::Recorded(fence.clone()))
            })?;
        if !self
            .exact_claim_is_current(&stream, &fence, Some(&authority))
            .await?
        {
            return Err(SmRegistryError::PromotionClaimLost(
                ShutdownSmClaimLoss::Recorded(fence),
            ));
        }
        if let Some(storage) = &self.persistence {
            self.mark_snapshot_stale(&stream)?;
            storage
                .acknowledge_claimed_session(&stream, h, &fence, Some(&authority))
                .await
                .map_err(|error| storage_error(error, &fence))?;
            // The storage call reuses our authority. Reconciliation is
            // read-only and preserves the latest claimed snapshot's identity.
            self.reconcile_stale_session_locked(&stream).await?;
            return self
                .detached_snapshot_matching(&stream, |session| {
                    session.promotion_custody.captured_fence() == Some(&fence)
                })?
                .ok_or_else(|| {
                    SmRegistryError::Internal("Committed resume ACK source disappeared".to_owned())
                });
        }
        let previous = session.last_acked;
        let acknowledged = h.wrapping_sub(previous);
        session.unacked_stanzas.retain(|entry| {
            let distance = entry.sequence.wrapping_sub(previous);
            distance == 0 || distance > acknowledged
        });
        session.last_acked = h;
        if !self.publish_detached_snapshot(&stream, session.clone())? {
            return Err(SmRegistryError::Internal(
                "Resume ACK source disappeared".to_owned(),
            ));
        }
        Ok(session)
    }

    /// Discharge durable ACK windows before promotion or source deletion.
    /// Each pending deletion commits before its exact evidence is removed.
    /// Never retain identity authority across a pending-backend await: that
    /// backend may acquire the same writer-preferring identity gate itself.
    pub async fn settle_session_acknowledgements(
        &self,
        session: &DetachedSession,
        pending: &dyn PendingDeliveryStorage,
    ) -> Result<(), SmRegistryError> {
        if self.persistence.is_none() {
            return Ok(());
        }
        let stream = SmSessionId::new(&session.stream_id);
        let lock = self.stream_lock(stream.as_str())?;
        let _guard = lock.lock().await;
        let fence = self.acknowledgement_fence_locked(session)?;
        self.settle_acknowledgements_locked(&stream, &fence, Some(pending))
            .await
    }

    /// Caller holds the stream shard. This also works when corrupt durable
    /// state prevents hydration, without weakening the captured ownership fence.
    pub(super) async fn settle_acknowledgements_locked(
        &self,
        stream: &SmSessionId,
        fence: &SmClaimFence,
        pending: Option<&dyn PendingDeliveryStorage>,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        let authority = self
            .node_identity
            .guard_if_current(fence.owner())
            .await
            .ok_or_else(|| {
                SmRegistryError::PromotionClaimLost(ShutdownSmClaimLoss::Recorded(fence.clone()))
            })?;
        if !self
            .exact_claim_is_current(stream, fence, Some(&authority))
            .await?
        {
            return Err(SmRegistryError::PromotionClaimLost(
                ShutdownSmClaimLoss::Recorded(fence.clone()),
            ));
        }
        let windows = storage
            .pending_acknowledgements(stream)
            .await
            .map_err(|error| storage_error(error, fence))?;
        drop(authority);
        if windows.is_empty() {
            return Ok(());
        }
        let pending = pending.ok_or(SmRegistryError::StorageUnavailable(
            super::traits::StorageOutageCause::Backend,
        ))?;
        for window in windows {
            pending
                .delete_acked_in_window(stream, window.from_exclusive, window.to_inclusive)
                .await
                .map_err(|error| match error {
                    crate::pending_delivery::storage::PendingStorageError::NotOwner { .. } => {
                        SmRegistryError::PromotionClaimLost(ShutdownSmClaimLoss::Recorded(
                            fence.clone(),
                        ))
                    }
                    error => SmRegistryError::Internal(error.to_string()),
                })?;
            let authority = self
                .node_identity
                .guard_if_current(fence.owner())
                .await
                .ok_or_else(|| {
                    SmRegistryError::PromotionClaimLost(ShutdownSmClaimLoss::Recorded(
                        fence.clone(),
                    ))
                })?;
            if !self
                .exact_claim_is_current(stream, fence, Some(&authority))
                .await?
            {
                return Err(SmRegistryError::PromotionClaimLost(
                    ShutdownSmClaimLoss::Recorded(fence.clone()),
                ));
            }
            storage
                .confirm_acknowledgement(stream, window, fence, Some(&authority))
                .await
                .map_err(|error| storage_error(error, fence))?;
        }
        Ok(())
    }

    /// Protect durable SM sources, attached claims, and retained ACK evidence
    /// during pending-claim recovery, including absent local registry state.
    /// Source/claim protection predates pending offers, avoiding a race with a
    /// later ACK journal insertion on another node.
    pub async fn pending_claim_protected_streams(
        &self,
    ) -> Result<Vec<SmSessionId>, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(Vec::new());
        };
        storage
            .pending_claim_protected_streams()
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))
    }
}
