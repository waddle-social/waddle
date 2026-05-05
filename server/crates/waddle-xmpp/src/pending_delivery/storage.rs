//! Persistence trait for `pending_delivery` (issue #209).
//!
//! Mirrors the inbox/MAM convention in this crate: the trait defines
//! the contract; an in-memory fake here serves handler tests; the real
//! libSQL/Postgres implementation lives in `waddle-server`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use jid::BareJid;

use super::{InsertOutcome, PendingRow, PendingRowId, QuotaPolicy, SmSessionId};

/// Errors returned by [`PendingDeliveryStorage`] implementations.
#[derive(Debug, thiserror::Error)]
pub enum PendingStorageError {
    #[error("pending_delivery storage error: {0}")]
    Other(String),
}

/// Storage contract for `pending_delivery`.
///
/// All operations are per-recipient (bare JID). FIFO ordering within a
/// recipient is implementation-mandated: `list()` returns rows in
/// insertion order so the flush wire shape preserves the sender's
/// order on replay.
#[async_trait]
pub trait PendingDeliveryStorage: Send + Sync {
    /// Insert a new pending row. Returns
    /// [`InsertOutcome::QuotaExceeded`] when the configured
    /// [`QuotaPolicy`] would be violated; the caller is then
    /// responsible for returning `<service-unavailable/>` per XEP-0160
    /// §3 step 3 (locked Q9b).
    async fn insert(&self, row: PendingRow) -> Result<InsertOutcome, PendingStorageError>;

    /// List all rows for `recipient`, FIFO. Includes rows currently
    /// claimed by another session (`flushed_in_session = Some(_)`) so
    /// callers can implement the Q7c re-flush path; pure flush callers
    /// should filter on `flushed_in_session.is_none()`.
    async fn list(&self, recipient: &BareJid) -> Result<Vec<PendingRow>, PendingStorageError>;

    /// Atomically claim every currently-unclaimed row for `recipient`,
    /// tagging it with `session`. Returns the rows that were claimed in
    /// FIFO order. Implements Q7c's per-user-bare-JID lock — concurrent
    /// `claim()` calls for the same recipient see each other's writes
    /// (only the first sees the un-claimed rows).
    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &SmSessionId,
    ) -> Result<Vec<PendingRow>, PendingStorageError>;

    /// Delete every row previously claimed by `session`. Used by paths
    /// that succeed or fail as a unit — e.g. SM-ack of the entire
    /// flush batch (locked Q7b).
    async fn delete_claimed(&self, session: &SmSessionId) -> Result<u64, PendingStorageError>;

    /// Delete a single row by id. Used by per-row partial-success
    /// flush paths so a delivered row is removed without affecting
    /// rows that failed to push.
    async fn delete_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError>;

    /// Release every row claimed by `session` back to the unclaimed
    /// pool. Used on SM-session expiry pre-ack so a subsequent
    /// recovering resource can re-flush them (Q7c re-flush path).
    async fn release_claim(&self, session: &SmSessionId) -> Result<u64, PendingStorageError>;

    /// Release a single row by id (clears `flushed_in_session`). Used
    /// by per-row partial-success flush paths so a row that failed to
    /// push becomes eligible for re-claim on the next flush trigger.
    async fn release_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError>;

    /// Current row count for `recipient` (used by the quota check;
    /// also exposed for metrics).
    async fn count(&self, recipient: &BareJid) -> Result<u32, PendingStorageError>;
}

/// In-memory implementation suitable for handler tests.
///
/// FIFO is preserved with a per-recipient `VecDeque<PendingRow>`.
/// Concurrency is bounded — every operation takes a single global
/// `Mutex` for simplicity. The libSQL implementation in
/// `waddle-server` will use row-level locking and a real index on
/// `(recipient, sequence)`.
#[derive(Debug)]
pub struct InMemoryPendingDeliveryStorage {
    inner: Mutex<HashMap<BareJid, VecDeque<PendingRow>>>,
    quota: QuotaPolicy,
}

impl InMemoryPendingDeliveryStorage {
    /// Build with the given quota policy.
    pub fn new(quota: QuotaPolicy) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            quota,
        }
    }

    /// Build with the default count cap (locked Q9e default).
    pub fn with_default_quota() -> Self {
        Self::new(QuotaPolicy::default_policy())
    }

    /// Build with no cap — useful for tests that don't exercise quota.
    pub fn unlimited() -> Self {
        Self::new(QuotaPolicy::Unlimited)
    }
}

impl Default for InMemoryPendingDeliveryStorage {
    fn default() -> Self {
        Self::with_default_quota()
    }
}

#[async_trait]
impl PendingDeliveryStorage for InMemoryPendingDeliveryStorage {
    async fn insert(&self, mut row: PendingRow) -> Result<InsertOutcome, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let entry = guard.entry(row.recipient.clone()).or_default();
        if let QuotaPolicy::CountCap { max_rows } = self.quota {
            if entry.len() as u32 >= max_rows {
                return Ok(InsertOutcome::QuotaExceeded);
            }
        }
        // Assign a fresh id if the caller didn't supply one. This lets
        // callers either pre-generate (for round-trip tests) or rely
        // on storage to do it (production path).
        if row.id.as_str().is_empty() {
            row.id = PendingRowId::fresh();
        }
        entry.push_back(row);
        Ok(InsertOutcome::Inserted)
    }

    async fn list(&self, recipient: &BareJid) -> Result<Vec<PendingRow>, PendingStorageError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        Ok(guard
            .get(recipient)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default())
    }

    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &SmSessionId,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let queue = match guard.get_mut(recipient) {
            Some(q) => q,
            None => return Ok(Vec::new()),
        };
        let mut claimed = Vec::new();
        for row in queue.iter_mut() {
            if row.flushed_in_session.is_none() {
                row.flushed_in_session = Some(session.clone());
                claimed.push(row.clone());
            }
        }
        Ok(claimed)
    }

    async fn delete_claimed(&self, session: &SmSessionId) -> Result<u64, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut removed = 0u64;
        for queue in guard.values_mut() {
            let before = queue.len();
            queue.retain(|row| row.flushed_in_session.as_ref() != Some(session));
            removed += (before - queue.len()) as u64;
        }
        guard.retain(|_, q| !q.is_empty());
        Ok(removed)
    }

    async fn delete_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut removed = 0u64;
        for queue in guard.values_mut() {
            let before = queue.len();
            queue.retain(|row| &row.id != id);
            removed += (before - queue.len()) as u64;
        }
        guard.retain(|_, q| !q.is_empty());
        Ok(removed)
    }

    async fn release_claim(&self, session: &SmSessionId) -> Result<u64, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut released = 0u64;
        for queue in guard.values_mut() {
            for row in queue.iter_mut() {
                if row.flushed_in_session.as_ref() == Some(session) {
                    row.flushed_in_session = None;
                    released += 1;
                }
            }
        }
        Ok(released)
    }

    async fn release_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        for queue in guard.values_mut() {
            for row in queue.iter_mut() {
                if &row.id == id {
                    row.flushed_in_session = None;
                    return Ok(1);
                }
            }
        }
        Ok(0)
    }

    async fn count(&self, recipient: &BareJid) -> Result<u32, PendingStorageError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        Ok(guard.get(recipient).map(|q| q.len() as u32).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pending_delivery::PendingPayload;
    use crate::protocol::event::{StanzaIdRef, StanzaIdValue};
    use chrono::Utc;
    use xmpp_parsers::message::Message;

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn archived_row(recipient: &str, id: &str) -> PendingRow {
        PendingRow {
            id: PendingRowId::fresh(),
            recipient: bare(recipient),
            original_receipt_at: Utc::now(),
            payload: PendingPayload::Archived(StanzaIdRef {
                by: bare(recipient),
                id: StanzaIdValue::new(id),
            }),
            flushed_in_session: None,
        }
    }

    fn transient_row(recipient: &str) -> PendingRow {
        PendingRow {
            id: PendingRowId::fresh(),
            recipient: bare(recipient),
            original_receipt_at: Utc::now(),
            payload: PendingPayload::Transient(Box::new(Message::new(None::<jid::Jid>))),
            flushed_in_session: None,
        }
    }

    #[tokio::test]
    async fn insert_and_list_preserves_fifo_order() {
        let store = InMemoryPendingDeliveryStorage::unlimited();
        for n in 0..5 {
            let outcome = store
                .insert(archived_row("alice@example.com", &format!("id-{n}")))
                .await
                .expect("insert ok");
            assert_eq!(outcome, InsertOutcome::Inserted);
        }
        let rows = store
            .list(&bare("alice@example.com"))
            .await
            .expect("list ok");
        assert_eq!(rows.len(), 5);
        for (n, row) in rows.iter().enumerate() {
            match &row.payload {
                PendingPayload::Archived(r) => assert_eq!(r.id.as_str(), format!("id-{n}")),
                _ => panic!("expected Archived"),
            }
        }
    }

    #[tokio::test]
    async fn quota_exceeded_returns_outcome() {
        let store = InMemoryPendingDeliveryStorage::new(QuotaPolicy::CountCap { max_rows: 2 });
        let recipient = "alice@example.com";
        for n in 0..2 {
            let outcome = store
                .insert(archived_row(recipient, &format!("id-{n}")))
                .await
                .expect("insert ok");
            assert_eq!(outcome, InsertOutcome::Inserted);
        }
        let outcome = store
            .insert(archived_row(recipient, "overflow"))
            .await
            .expect("insert ok");
        assert_eq!(outcome, InsertOutcome::QuotaExceeded);
        // Existing rows preserved (XEP-0160 §3 step 3 — refuse new, keep old).
        assert_eq!(store.count(&bare(recipient)).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn claim_marks_rows_for_session_first_caller_wins() {
        let store = InMemoryPendingDeliveryStorage::unlimited();
        for n in 0..3 {
            store
                .insert(archived_row("alice@example.com", &format!("id-{n}")))
                .await
                .expect("insert ok");
        }
        let session1 = SmSessionId::new("session-1");
        let session2 = SmSessionId::new("session-2");
        let claimed1 = store
            .claim_for_session(&bare("alice@example.com"), &session1)
            .await
            .expect("claim ok");
        let claimed2 = store
            .claim_for_session(&bare("alice@example.com"), &session2)
            .await
            .expect("claim ok");
        assert_eq!(claimed1.len(), 3);
        assert_eq!(claimed2.len(), 0); // first caller drained the unclaimed pool
    }

    #[tokio::test]
    async fn delete_claimed_removes_only_session_rows() {
        let store = InMemoryPendingDeliveryStorage::unlimited();
        let recipient = bare("alice@example.com");
        store
            .insert(archived_row("alice@example.com", "a"))
            .await
            .unwrap();
        store
            .insert(archived_row("alice@example.com", "b"))
            .await
            .unwrap();
        store
            .insert(archived_row("alice@example.com", "c"))
            .await
            .unwrap();

        let session = SmSessionId::new("s1");
        let _ = store.claim_for_session(&recipient, &session).await.unwrap();

        let removed = store.delete_claimed(&session).await.unwrap();
        assert_eq!(removed, 3);
        assert_eq!(store.count(&recipient).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn release_claim_makes_rows_eligible_for_reflush() {
        let store = InMemoryPendingDeliveryStorage::unlimited();
        let recipient = bare("alice@example.com");
        store
            .insert(archived_row("alice@example.com", "a"))
            .await
            .unwrap();
        store
            .insert(archived_row("alice@example.com", "b"))
            .await
            .unwrap();

        let session1 = SmSessionId::new("s1");
        let claimed = store
            .claim_for_session(&recipient, &session1)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 2);

        // Session dies pre-ack — release.
        let released = store.release_claim(&session1).await.unwrap();
        assert_eq!(released, 2);

        // A new session can now claim.
        let session2 = SmSessionId::new("s2");
        let reclaimed = store
            .claim_for_session(&recipient, &session2)
            .await
            .unwrap();
        assert_eq!(reclaimed.len(), 2);
    }

    #[tokio::test]
    async fn transient_payload_round_trips() {
        let store = InMemoryPendingDeliveryStorage::unlimited();
        store
            .insert(transient_row("alice@example.com"))
            .await
            .unwrap();
        let rows = store.list(&bare("alice@example.com")).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].payload.is_transient());
    }

    #[tokio::test]
    async fn empty_recipient_count_is_zero() {
        let store = InMemoryPendingDeliveryStorage::unlimited();
        assert_eq!(store.count(&bare("nobody@example.com")).await.unwrap(), 0);
    }
}
