//! Incremental detached replay writes. Queue history is never reparsed or rewritten.

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::{
    KeyedSnapshotOutcome, PersistedDetachedAppend, PersistedUnackedStanza,
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
        // Only storage work is asynchronous. Owning the operation guard in this
        // task keeps cancellation from exposing a still-committing delta to the
        // next writer. The stale marker makes an uncertain result reload first.
        let _guard = if let Some(storage) = &self.persistence {
            let append = prepare_detached_append(
                &previous_sequences,
                &updated,
                sequence,
                stanza,
                original_receipt_at,
            )?;
            let fence = updated.promotion_custody.captured_fence().cloned();
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
