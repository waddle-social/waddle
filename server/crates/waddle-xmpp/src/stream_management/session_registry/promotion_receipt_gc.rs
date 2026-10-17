//! Source-proven cleanup for portable handoff receipts in an independent store.

use crate::pending_delivery::storage::PendingDeliveryStorage;
use crate::pending_delivery::SmSessionId;

use super::{InMemorySmSessionRegistry, SmRegistryError};

impl InMemorySmSessionRegistry {
    /// Retire an orphan's portable receipts only after the canonical source is
    /// absent and this registry holds no remaining responsibility for its ID.
    /// Candidate enumeration must precede this check; age alone is not proof.
    /// Stream IDs are immutable: a retired ID cannot admit another lifecycle.
    pub async fn retire_orphaned_promotion_receipts(
        &self,
        stream_id: &SmSessionId,
        pending_storage: &dyn PendingDeliveryStorage,
    ) -> Result<bool, SmRegistryError> {
        let Some(persistence) = &self.persistence else {
            // A memory-only registry cannot establish the status of an
            // independently configured durable source database.
            return Ok(false);
        };
        let shard = self.stream_lock(stream_id.as_str())?;
        let _guard = shard.lock().await;
        if self.has_local_receipt_source_custody(stream_id)? {
            return Ok(false);
        }
        if persistence
            .get_session(stream_id)
            .await
            .map_err(|error| {
                SmRegistryError::Internal(format!("Cannot prove promotion source absent: {error}"))
            })?
            .is_some()
        {
            return Ok(false);
        }
        // Registration publishes custody before waiting for the shard. Check
        // again after the durable await, including off-map retry/admission
        // inventories; poisoned bookkeeping is never an empty source set.
        if self.has_local_receipt_source_custody(stream_id)? {
            return Ok(false);
        }
        pending_storage
            .confirm_promotion(stream_id)
            .await
            .map_err(|error| {
                SmRegistryError::Internal(format!(
                    "Cannot retire orphan promotion receipts: {error}"
                ))
            })?;
        Ok(true)
    }

    fn has_local_receipt_source_custody(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<bool, SmRegistryError> {
        let owned = self.locally_owned_claim_ids().ok_or_else(|| {
            SmRegistryError::Internal("Cannot inspect promotion source custody".to_owned())
        })?;
        Ok(owned.iter().any(|id| id == stream_id.as_str()))
    }
}
