//! Persistence trait for `pending_delivery` (issue #209).
//!
//! Mirrors the inbox/MAM convention in this crate: the trait defines
//! the contract; an in-memory fake here serves handler tests; the real
//! libSQL/Postgres implementation lives in `waddle-server`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use jid::BareJid;

use crate::ownership::Entity;
use crate::postgres_identity::ClusterColocationIdentities;

use super::{
    InsertOutcome, PendingRow, PendingRowId, QuotaPolicy, SmSessionId, TombstoneScrubbedPendingRows,
};

/// Errors returned by [`PendingDeliveryStorage`] implementations.
#[derive(Debug, thiserror::Error)]
pub enum PendingStorageError {
    #[error("pending_delivery storage error: {0}")]
    Other(String),

    /// ADR-0017 Phase 3 Slice 5 FIX 3 (council-adjudicated): a fenced
    /// `insert_fenced` call's own `SELECT ... FOR SHARE` fencing check
    /// (against `clustering_claims`, mirroring
    /// `SmPersistenceError::NotOwner`/`sm_persistence_fenced`'s identical
    /// pattern one table over) observed that this node does not hold — or
    /// never acquired — the origin SM session's ownership claim at the
    /// epoch it believed was current. The write was rolled back before
    /// touching `pending_delivery`. Only ever returned by a
    /// cluster-fenced implementation; the portable, single-node
    /// implementation has no fencing concept and never returns this.
    #[error(
        "fencing check failed: this node does not hold entity '{entity}' at the expected claim epoch"
    )]
    NotOwner { entity: Entity },

    /// ADR-0017 Phase 3 Slice 5 FIX 3: clustered fencing for
    /// `pending_delivery` requires this storage's own database to be
    /// co-located with the clustering global database (the fencing
    /// `SELECT ... FOR SHARE` targets `clustering_claims`, which only
    /// exists there) — mirroring
    /// `SmPersistenceError::ClusterColocationMismatch`'s identical
    /// invariant for `PostgresFencedSmPersistence`.
    #[error(
        "clustered pending_delivery fencing must be co-located with the clustering claims \
         tables: physical pending_delivery storage identity does not match the clustering global \
         database identity"
    )]
    ClusterColocationMismatch {
        identities: Box<ClusterColocationIdentities>,
    },
}

/// Outcome of releasing sequence-bound pending rows during terminal recovery.
///
/// `released` names the replay entries that are no longer owned by the
/// detached session and can therefore be stripped from its in-memory replay
/// queue immediately. `error` carries any backend failure that interrupted the
/// sweep after some rows may already have been released.
#[derive(Debug)]
pub struct ReleaseRowsForOutboundSequencesOutcome {
    pub released: HashSet<u32>,
    pub error: Option<PendingStorageError>,
}

impl ReleaseRowsForOutboundSequencesOutcome {
    pub fn complete(released: HashSet<u32>) -> Self {
        Self {
            released,
            error: None,
        }
    }

    pub fn failed(error: PendingStorageError) -> Self {
        Self {
            released: HashSet::new(),
            error: Some(error),
        }
    }

    pub fn partial(released: HashSet<u32>, error: PendingStorageError) -> Self {
        Self {
            released,
            error: Some(error),
        }
    }
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

    /// Fenced variant of [`Self::insert`] for the XEP-0198 §5 Q6 promotion
    /// write path (ADR-0017 Phase 3 Slice 5 FIX 3, council-adjudicated):
    /// element 9's locked text requires "promotion executes under the
    /// row-locked fenced epoch" of the origin SM session
    /// (`origin_stream_id`) whose unacked queue is being promoted, so two
    /// nodes double-janitoring the same expired session can never both
    /// commit the same stanza into `pending_delivery`.
    ///
    /// Default impl ignores `origin_stream_id` and falls back to
    /// [`Self::insert`] — correct for every implementation with no
    /// clustering/fencing concept (the portable, single-node backend, and
    /// the in-memory test double). A cluster-aware implementation
    /// overrides this to run the fencing `SELECT ... FOR SHARE` check
    /// (against `clustering_claims`) and the insert in one transaction,
    /// mirroring `sm_persistence_fenced`'s identical pattern one table
    /// over — see that module's doc comment for the full design. On a
    /// failed fence, returns [`PendingStorageError::NotOwner`] and the
    /// write never touches `pending_delivery`.
    async fn insert_fenced(
        &self,
        row: PendingRow,
        origin_stream_id: &str,
    ) -> Result<InsertOutcome, PendingStorageError> {
        let _ = origin_stream_id;
        self.insert(row).await
    }

    /// List all rows for `recipient`, FIFO. Includes rows currently
    /// claimed by another session (`flushed_in_session = Some(_)`) so
    /// callers can implement the Q7c re-flush path; pure flush callers
    /// should filter on `flushed_in_session.is_none()`.
    async fn list(&self, recipient: &BareJid) -> Result<Vec<PendingRow>, PendingStorageError>;

    /// List a bounded page of unclaimed rows for `recipient`, FIFO, optionally
    /// starting after `after`.
    ///
    /// This is the scalable read path for background reconciliation tasks that
    /// only need a small prefix and must not materialize a recipient's entire
    /// offline backlog. Implementations with a real database should override
    /// this with `WHERE flushed_in_session IS NULL ... LIMIT ?`; the default
    /// keeps older test doubles correct by filtering [`Self::list`].
    async fn list_unclaimed_after(
        &self,
        recipient: &BareJid,
        after: Option<&PendingRowId>,
        limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let after = after.map(PendingRowId::as_str);
        let mut rows = self
            .list(recipient)
            .await?
            .into_iter()
            .filter(|row| row.flushed_in_session.is_none())
            .filter(|row| after.is_none_or(|after| row.id.as_str() > after))
            .collect::<Vec<_>>();
        rows.truncate(limit);
        Ok(rows)
    }

    /// List a bounded global page of unclaimed Archived rows that have
    /// not yet been acknowledged by the notification candidate pipeline.
    ///
    /// This is intentionally global rather than per-recipient: the
    /// recovery janitor must be able to find crash gaps after a durable
    /// XEP-0160 pending row was committed but before the durable XEP-0357
    /// candidate/outbox write completed. Implementations that do not
    /// support this recovery path may keep the default empty page.
    async fn list_unoutboxed_archived(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        let _ = limit;
        Ok(Vec::new())
    }

    /// Mark an Archived pending row as having completed notification
    /// candidate handling. Returns the number of rows marked.
    async fn mark_notification_outboxed(
        &self,
        id: &PendingRowId,
    ) -> Result<u64, PendingStorageError> {
        let _ = id;
        Ok(0)
    }

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

    /// Atomically claim only the next FIFO **prefix** of currently-unclaimed
    /// rows for `recipient` — at most `limit` rows whose `row_id` sorts
    /// strictly after `after` — tagging each with `session`. Returns the
    /// claimed rows in FIFO (`row_id ASC`) order; row ids are UUID v7, so
    /// that order reproduces order of receipt.
    ///
    /// This is the bounded sibling of [`Self::claim_for_session`], used by the
    /// batched offline flush (issue #1220) so a large backlog drains in
    /// `FLUSH_BATCH_SIZE` chunks that stay well under the recipient's outbound
    /// mpsc capacity, instead of the whole queue landing wholesale in the SM
    /// unacked queue.
    ///
    /// `after` is a FIFO cursor advancing batch-to-batch progress: pass `None`
    /// for the first batch, then the last claimed row's id for each subsequent
    /// batch so the next batch starts strictly after it.
    ///
    /// CORRECTNESS INVARIANT: an implementation MUST return ONLY the rows it
    /// just transitioned from unclaimed — never a row already claimed by an
    /// earlier call. This matters because the SM flush stamps
    /// `outbound_sequence` asynchronously, on the recipient's connection task
    /// (see [`Self::record_pushed_at`]), so a prior flush pass's rows can
    /// linger as `flushed_in_session = session, outbound_sequence = NULL`; a
    /// query keyed only on `(session, outbound_sequence IS NULL)` would
    /// re-return and double-deliver them on a `reset_offline_flush` retry.
    /// The in-memory backend upholds this by only claiming rows whose
    /// `flushed_in_session.is_none()`; the SQL backend uses
    /// `UPDATE … RETURNING`, which yields exactly the transitioned rows.
    /// `limit == 0` claims nothing.
    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &SmSessionId,
        after: Option<&PendingRowId>,
        limit: usize,
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

    /// Release a single row only when it is still claimed by
    /// `expected_session`. Used by the claim-expiry janitor (issue
    /// #209 finding #9): the (row_id, session) pairs returned by
    /// [`Self::list_orphaned_claims`] reflect a snapshot of the
    /// live-set taken seconds ago. Between the snapshot and the
    /// release, a fresh bind on a different recipient session can
    /// have re-claimed the row under a now-live session. An
    /// unconditional `release_row` would clear that fresh claim and
    /// wedge the row (the new session's SM ack would skip it because
    /// `outbound_sequence` is NULL).
    ///
    /// Returns the number of rows actually updated — `0` when the
    /// row's `flushed_in_session` no longer matches `expected_session`
    /// (i.e. someone else re-claimed it; leave it alone) or the row
    /// has been deleted.
    ///
    /// Default impl falls back to the unconditional [`Self::release_row`]
    /// path so in-memory backends keep working without re-implementing
    /// the conditional check; the libSQL/Postgres backend overrides
    /// with `UPDATE … WHERE row_id = ? AND flushed_in_session = ?`
    /// so the per-row re-validation is atomic with the update.
    async fn release_row_if_session(
        &self,
        id: &PendingRowId,
        expected_session: &SmSessionId,
    ) -> Result<u64, PendingStorageError> {
        let _ = expected_session;
        self.release_row(id).await
    }

    /// Release only rows whose recorded outbound sequence belongs to a
    /// terminally-promoted SM queue. This is the inverse of
    /// [`Self::delete_acked_in_window`]: terminal recovery abandons replay,
    /// so a row that already has a durable `(session, sequence)` binding must
    /// return to ordinary pending-delivery redelivery instead of being
    /// promoted from its unacked XML a second time.
    ///
    /// The default keeps storage implementations source-compatible while
    /// preserving the ownership check through [`Self::release_row_if_session`].
    /// Implementations may override it with a set-based query.
    async fn release_rows_for_outbound_sequences(
        &self,
        recipient: &BareJid,
        session: &SmSessionId,
        sequences: &HashSet<u32>,
    ) -> ReleaseRowsForOutboundSequencesOutcome {
        if sequences.is_empty() {
            return ReleaseRowsForOutboundSequencesOutcome::complete(HashSet::new());
        }

        let rows = match self.list(recipient).await {
            Ok(rows) => rows,
            Err(error) => return ReleaseRowsForOutboundSequencesOutcome::failed(error),
        };
        let mut released = HashSet::new();
        for row in rows.into_iter().filter(|row| {
            row.flushed_in_session.as_ref() == Some(session)
                && row
                    .outbound_sequence
                    .is_some_and(|sequence| sequences.contains(&sequence))
        }) {
            let released_count = match self.release_row_if_session(&row.id, session).await {
                Ok(released_count) => released_count,
                Err(error) => {
                    return ReleaseRowsForOutboundSequencesOutcome::partial(released, error);
                }
            };
            if released_count > 0 {
                if let Some(sequence) = row.outbound_sequence {
                    released.insert(sequence);
                }
            }
        }
        ReleaseRowsForOutboundSequencesOutcome::complete(released)
    }

    /// Stamp the XEP-0198 outbound counter value onto a previously-
    /// claimed row, after that row's flush stanza has been pushed
    /// onto the recovering session's outbound queue and assigned its
    /// SM outbound sequence (locked Q7b). Pair with
    /// [`Self::delete_acked_in_window`]: an SM `<a h='N'/>` ack from
    /// the recovering session range-deletes claimed rows whose
    /// `outbound_sequence` lies in the newly-acknowledged mod-2^32
    /// window `(last_acked, N]`.
    async fn record_pushed_at(
        &self,
        id: &PendingRowId,
        sequence: u32,
    ) -> Result<u64, PendingStorageError>;

    /// Range-delete rows previously claimed by `session` whose
    /// recorded `outbound_sequence` lies in the mod-2^32 ack interval
    /// `(from_exclusive, to_inclusive]` (locked Q7b SM-ack-keyed
    /// deletion). The SM ack handler invokes this with the
    /// pre-acknowledge `last_acked` as `from_exclusive` and the `h`
    /// value carried in the ack as `to_inclusive`, so only stanzas the
    /// recovering session has actually acknowledged are removed; rows
    /// whose flush stanzas haven't yet been ack'd stay claimed for a
    /// future ack.
    ///
    /// The interval is WRAP-AWARE: XEP-0198 counters are mod 2^32, so
    /// a valid wrap-spanning ack (`h` numerically small post-wrap)
    /// must also delete the pre-wrap rows near `u32::MAX` — see
    /// [`sequence_in_ack_window`] for the shared predicate. An empty
    /// window (`from_exclusive == to_inclusive`) deletes nothing.
    ///
    /// Rows with `outbound_sequence = NULL` (claimed but not yet
    /// pushed) are intentionally NOT deleted by this call — they are
    /// either still in the push pipeline or were claimed by a session
    /// that died pre-push (handled by [`Self::release_claim`]).
    async fn delete_acked_in_window(
        &self,
        session: &SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, PendingStorageError>;

    /// List rows whose `flushed_in_session` references a session
    /// that is NOT in `live_sessions` AND whose claim was stamped at
    /// or before `claimed_before_ms`. Used by the claim-expiry
    /// janitor (issue #209 PR #360) to find orphaned claims left
    /// behind by sessions that closed without going through the SM
    /// janitor / shutdown drain (e.g. non-SM sessions, or SM
    /// sessions that crashed before `store_session`). The janitor
    /// then calls [`Self::release_row`] on each entry to make the
    /// rows eligible for re-flush.
    ///
    /// `claimed_before_ms` is the recency floor (#1124): non-SM
    /// flushes claim rows under a synthetic `transient:` session id
    /// that is never in the live-set, so without the floor any
    /// janitor pass overlapping an in-flight flush would release its
    /// claims mid-flight and let a second resource re-push the same
    /// offline messages. Claims stamped after the floor are
    /// considered in-flight and skipped. A claim with NO stamp is
    /// also skipped — an unstamped claim means "recency unknown"
    /// (e.g. written by a pre-#1124 binary during a rolling deploy),
    /// and treating unknown as old would re-open the mid-flight
    /// release. Callers first adopt unstamped claims via
    /// [`Self::stamp_unstamped_claims`], which starts their floor
    /// clock, so they become release-eligible one floor-window later.
    ///
    /// Implementations MUST scan only rows with
    /// `flushed_in_session IS NOT NULL`. The caller passes a
    /// snapshot of currently-live SM session ids; an empty
    /// `live_sessions` slice matches every claimed row older than
    /// the floor (useful for startup recovery when the SM registry
    /// is empty).
    async fn list_orphaned_claims(
        &self,
        live_sessions: &[SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<Vec<(PendingRowId, SmSessionId)>, PendingStorageError>;

    /// Stamp `now_ms` onto every claimed row that has no claim-recency
    /// stamp, returning the number of rows stamped. The claim-expiry
    /// janitor calls this before [`Self::list_orphaned_claims`] so a
    /// claim written without a stamp (a pre-#1124 binary during a
    /// rolling deploy) is adopted into the recency floor instead of
    /// being either released mid-flight (unknown treated as old) or
    /// leaked forever (unknown treated as always-fresh). Adopted
    /// claims become release-eligible one floor-window after adoption.
    ///
    /// Default impl is a no-op: backends that always stamp on claim
    /// (every in-process backend) have nothing to adopt.
    async fn stamp_unstamped_claims(&self, now_ms: i64) -> Result<u64, PendingStorageError> {
        let _ = now_ms;
        Ok(0)
    }

    /// Current row count for `recipient` (used by the quota check;
    /// also exposed for metrics).
    async fn count(&self, recipient: &BareJid) -> Result<u32, PendingStorageError>;

    /// Delete every row whose `original_receipt_at < cutoff`.
    /// Returns the number of rows removed.
    ///
    /// Used by the pending_delivery aging janitor (issue #209
    /// finding #5): without an upper age bound, a permanently-
    /// offline recipient (deleted account, lost device) eventually
    /// fills their per-recipient quota with stale rows that block
    /// future legitimate senders forever via the
    /// `<service-unavailable/>` bounce. Rows older than the
    /// operator-defined threshold (default 30 days) are dropped.
    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, PendingStorageError>;

    /// Remove every pending row matching a XEP-0424 / XEP-0425
    /// tombstone, returning the number of rows removed.
    ///
    /// Promotion (#1097/#1098) parks unacked stanzas here, so the
    /// retraction/moderation scrub must reach this layer too or the
    /// retracted content delivers verbatim at the recipient's next
    /// login. Matching mirrors [`crate::tombstone`]:
    ///
    /// - `Transient` rows carry the message inline — matched by the
    ///   shared typed predicate
    ///   ([`crate::tombstone::TombstoneTarget::matches_message_element`]:
    ///   room stanza-id for groupchat, author-scoped wire id for 1:1,
    ///   both scoped to the conversation archive).
    /// - `Archived` rows are MAM pointers — matched by exact
    ///   `(stanza_id.id == target.id(), stanza_id.by bare-equals
    ///   target.archive_jid())`; the MAM row itself has been
    ///   tombstoned, so the pointer must not flush a stub for a
    ///   message the recipient never saw.
    async fn scrub_for_tombstone(
        &self,
        target: &crate::tombstone::TombstoneTarget,
    ) -> Result<u64, PendingStorageError>;

    /// Typed sibling of [`Self::scrub_for_tombstone`] that returns exact row
    /// identities when the implementation can provide them.
    ///
    /// Default impl preserves source compatibility for older backends by
    /// delegating to the existing count-only method and returning no ids.
    async fn scrub_for_tombstone_with_row_ids(
        &self,
        target: &crate::tombstone::TombstoneTarget,
    ) -> Result<TombstoneScrubbedPendingRows, PendingStorageError> {
        Ok(TombstoneScrubbedPendingRows::count_only(
            self.scrub_for_tombstone(target).await?,
        ))
    }

    /// Periodically GC backend-internal bookkeeping that grows with
    /// distinct keys seen by the process — e.g. per-recipient
    /// insert-serialization locks (issue #209 finding #4). Default
    /// impl is a no-op for backends that don't need it (in-memory).
    /// Returns the number of entries removed for observability.
    fn sweep_internal_bookkeeping(&self) -> usize {
        0
    }
}

/// Shared per-row tombstone predicate for [`PendingDeliveryStorage`]
/// implementations that hold typed [`PendingRow`]s (the in-memory
/// backend here; SQL backends match on their column representation
/// with the same semantics).
pub fn pending_row_matches_tombstone(
    row: &PendingRow,
    target: &crate::tombstone::TombstoneTarget,
) -> bool {
    match &row.payload {
        super::PendingPayload::Transient(message) => {
            let element: xmpp_parsers::minidom::Element = (**message).clone().into();
            target.matches_message_element(&element)
        }
        super::PendingPayload::Archived(stanza_id) => {
            stanza_id.id.as_str() == target.id() && &stanza_id.by.to_bare() == target.archive_jid()
        }
    }
}

/// Shared mod-2^32 ack-window predicate for
/// [`PendingDeliveryStorage::delete_acked_in_window`] implementations:
/// is `seq` inside the interval `(from_exclusive, to_inclusive]` on the
/// XEP-0198 counter circle?
///
/// Numerically: `seq > from && seq <= to` when `from <= to`, else (the
/// window spans the u32 wrap) `seq > from || seq <= to`. An empty
/// window (`from == to`) contains nothing. SQL backends mirror this
/// exact two-branch expression on their `outbound_sequence` column.
pub fn sequence_in_ack_window(seq: u32, from_exclusive: u32, to_inclusive: u32) -> bool {
    if from_exclusive <= to_inclusive {
        seq > from_exclusive && seq <= to_inclusive
    } else {
        seq > from_exclusive || seq <= to_inclusive
    }
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
    notification_outboxed: Mutex<HashSet<PendingRowId>>,
    /// Claim recency stamps (#1124): row id → `timestamp_millis()` of
    /// the claim that set `flushed_in_session`. Kept beside the rows
    /// (not on [`PendingRow`]) because no consumer of the row needs
    /// it — only [`PendingDeliveryStorage::list_orphaned_claims`]
    /// reads it, to skip in-flight claims. Cleared on release; a
    /// missing entry means "no recency information" and the claim is
    /// always release-eligible.
    claimed_at_ms: Mutex<HashMap<PendingRowId, i64>>,
    quota: QuotaPolicy,
}

impl InMemoryPendingDeliveryStorage {
    /// Build with the given quota policy.
    pub fn new(quota: QuotaPolicy) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            notification_outboxed: Mutex::new(HashSet::new()),
            claimed_at_ms: Mutex::new(HashMap::new()),
            quota,
        }
    }

    fn stamp_claimed_at(&self, ids: &[PendingRowId]) -> Result<(), PendingStorageError> {
        if ids.is_empty() {
            return Ok(());
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut stamps = self
            .claimed_at_ms
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        for id in ids {
            stamps.insert(id.clone(), now_ms);
        }
        Ok(())
    }

    fn clear_claimed_at(&self, ids: &[PendingRowId]) -> Result<(), PendingStorageError> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut stamps = self
            .claimed_at_ms
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        for id in ids {
            stamps.remove(id);
        }
        Ok(())
    }

    /// Build with the default count cap (locked Q9e default).
    pub fn with_default_quota() -> Self {
        Self::new(QuotaPolicy::default_policy())
    }

    /// Build with no cap — useful for tests that don't exercise quota.
    pub fn unlimited() -> Self {
        Self::new(QuotaPolicy::Unlimited)
    }

    fn clear_notification_outboxed_markers(
        &self,
        ids: &[PendingRowId],
    ) -> Result<(), PendingStorageError> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut notification_outboxed = self
            .notification_outboxed
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        for id in ids {
            notification_outboxed.remove(id);
        }
        Ok(())
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

    async fn list_unclaimed_after(
        &self,
        recipient: &BareJid,
        after: Option<&PendingRowId>,
        limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let after = after.map(PendingRowId::as_str);
        Ok(guard
            .get(recipient)
            .map(|q| {
                q.iter()
                    .filter(|row| row.flushed_in_session.is_none())
                    .filter(|row| after.is_none_or(|after| row.id.as_str() > after))
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn list_unoutboxed_archived(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let notification_outboxed = self
            .notification_outboxed
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut rows = guard
            .values()
            .flat_map(|queue| queue.iter())
            .filter(|row| row.flushed_in_session.is_none())
            .filter(|row| row.payload.is_archived())
            .filter(|row| !notification_outboxed.contains(&row.id))
            .cloned()
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        rows.truncate(limit);
        Ok(rows)
    }

    async fn mark_notification_outboxed(
        &self,
        id: &PendingRowId,
    ) -> Result<u64, PendingStorageError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        if !guard
            .values()
            .flat_map(|queue| queue.iter())
            .any(|row| &row.id == id)
        {
            return Ok(0);
        }
        drop(guard);
        let mut notification_outboxed = self
            .notification_outboxed
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        Ok(u64::from(notification_outboxed.insert(id.clone())))
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
                // Defensive: clear any leftover sequence so the new
                // claim starts from a known-clean state. release_*
                // already does this, but a future code path that
                // leaves a row half-released should not be able to
                // confuse the SM-ack delete.
                row.outbound_sequence = None;
                claimed.push(row.clone());
            }
        }
        drop(guard);
        let claimed_ids: Vec<PendingRowId> = claimed.iter().map(|row| row.id.clone()).collect();
        self.stamp_claimed_at(&claimed_ids)?;
        Ok(claimed)
    }

    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &SmSessionId,
        after: Option<&PendingRowId>,
        limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let queue = match guard.get_mut(recipient) {
            Some(q) => q,
            None => return Ok(Vec::new()),
        };
        let after = after.map(PendingRowId::as_str);
        // Take the FIFO prefix of unclaimed rows strictly after the cursor,
        // ordered by `row_id` to match the SQL backend's canonical
        // `ORDER BY row_id ASC` (UUID v7 → time-sortable). Ordering by id
        // rather than by `VecDeque` insertion position keeps in-memory and
        // SQL agreeing even for rows minted within the same millisecond.
        let mut candidates: Vec<usize> = queue
            .iter()
            .enumerate()
            .filter(|(_, row)| row.flushed_in_session.is_none())
            .filter(|(_, row)| after.is_none_or(|cursor| row.id.as_str() > cursor))
            .map(|(idx, _)| idx)
            .collect();
        candidates.sort_by(|&a, &b| queue[a].id.as_str().cmp(queue[b].id.as_str()));
        candidates.truncate(limit);
        let mut claimed = Vec::with_capacity(candidates.len());
        for idx in candidates {
            let row = &mut queue[idx];
            row.flushed_in_session = Some(session.clone());
            row.outbound_sequence = None;
            claimed.push(row.clone());
        }
        drop(guard);
        let claimed_ids: Vec<PendingRowId> = claimed.iter().map(|row| row.id.clone()).collect();
        self.stamp_claimed_at(&claimed_ids)?;
        Ok(claimed)
    }

    async fn delete_claimed(&self, session: &SmSessionId) -> Result<u64, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut removed = 0u64;
        let mut removed_ids = Vec::new();
        for queue in guard.values_mut() {
            let mut kept = VecDeque::with_capacity(queue.len());
            for row in queue.drain(..) {
                if row.flushed_in_session.as_ref() == Some(session) {
                    removed_ids.push(row.id);
                    removed += 1;
                } else {
                    kept.push_back(row);
                }
            }
            *queue = kept;
        }
        guard.retain(|_, q| !q.is_empty());
        drop(guard);
        self.clear_notification_outboxed_markers(&removed_ids)?;
        self.clear_claimed_at(&removed_ids)?;
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
        drop(guard);
        if removed > 0 {
            self.clear_notification_outboxed_markers(std::slice::from_ref(id))?;
            self.clear_claimed_at(std::slice::from_ref(id))?;
        }
        Ok(removed)
    }

    async fn release_claim(&self, session: &SmSessionId) -> Result<u64, PendingStorageError> {
        // Clear `outbound_sequence` alongside `flushed_in_session` so
        // a stale sequence from the dead session can't survive a
        // re-claim and trick a later session's SM ack into deleting
        // an unack'd row. (Qodo review on PR #358.)
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut released = 0u64;
        let mut released_ids = Vec::new();
        for queue in guard.values_mut() {
            for row in queue.iter_mut() {
                if row.flushed_in_session.as_ref() == Some(session) {
                    row.flushed_in_session = None;
                    row.outbound_sequence = None;
                    released_ids.push(row.id.clone());
                    released += 1;
                }
            }
        }
        drop(guard);
        self.clear_claimed_at(&released_ids)?;
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
                    row.outbound_sequence = None;
                    drop(guard);
                    self.clear_claimed_at(std::slice::from_ref(id))?;
                    return Ok(1);
                }
            }
        }
        Ok(0)
    }

    async fn release_row_if_session(
        &self,
        id: &PendingRowId,
        expected_session: &SmSessionId,
    ) -> Result<u64, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        for queue in guard.values_mut() {
            for row in queue.iter_mut() {
                if &row.id == id {
                    if row.flushed_in_session.as_ref() != Some(expected_session) {
                        return Ok(0);
                    }
                    row.flushed_in_session = None;
                    row.outbound_sequence = None;
                    drop(guard);
                    self.clear_claimed_at(std::slice::from_ref(id))?;
                    return Ok(1);
                }
            }
        }
        Ok(0)
    }

    async fn release_rows_for_outbound_sequences(
        &self,
        recipient: &BareJid,
        session: &SmSessionId,
        sequences: &HashSet<u32>,
    ) -> ReleaseRowsForOutboundSequencesOutcome {
        if sequences.is_empty() {
            return ReleaseRowsForOutboundSequencesOutcome::complete(HashSet::new());
        }

        let guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()));
        let mut guard = match guard {
            Ok(guard) => guard,
            Err(error) => return ReleaseRowsForOutboundSequencesOutcome::failed(error),
        };
        let Some(queue) = guard.get_mut(recipient) else {
            return ReleaseRowsForOutboundSequencesOutcome::complete(HashSet::new());
        };

        let mut released_ids = Vec::new();
        let mut released_sequences = HashSet::new();
        for row in queue.iter_mut() {
            if row.flushed_in_session.as_ref() == Some(session)
                && row
                    .outbound_sequence
                    .is_some_and(|sequence| sequences.contains(&sequence))
            {
                released_ids.push(row.id.clone());
                released_sequences.extend(row.outbound_sequence);
                row.flushed_in_session = None;
                row.outbound_sequence = None;
            }
        }
        drop(guard);
        if let Err(error) = self.clear_claimed_at(&released_ids) {
            return ReleaseRowsForOutboundSequencesOutcome::partial(released_sequences, error);
        }
        ReleaseRowsForOutboundSequencesOutcome::complete(released_sequences)
    }

    async fn record_pushed_at(
        &self,
        id: &PendingRowId,
        sequence: u32,
    ) -> Result<u64, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        for queue in guard.values_mut() {
            for row in queue.iter_mut() {
                if &row.id == id {
                    row.outbound_sequence = Some(sequence);
                    return Ok(1);
                }
            }
        }
        Ok(0)
    }

    async fn delete_acked_in_window(
        &self,
        session: &SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut removed = 0u64;
        let mut removed_ids = Vec::new();
        for queue in guard.values_mut() {
            let mut kept = VecDeque::with_capacity(queue.len());
            for row in queue.drain(..) {
                let claimed_by_session = row.flushed_in_session.as_ref() == Some(session);
                let acked = matches!(
                    row.outbound_sequence,
                    Some(seq) if sequence_in_ack_window(seq, from_exclusive, to_inclusive)
                );
                if claimed_by_session && acked {
                    removed_ids.push(row.id);
                    removed += 1;
                } else {
                    kept.push_back(row);
                }
            }
            *queue = kept;
        }
        guard.retain(|_, q| !q.is_empty());
        drop(guard);
        self.clear_notification_outboxed_markers(&removed_ids)?;
        self.clear_claimed_at(&removed_ids)?;
        Ok(removed)
    }

    async fn list_orphaned_claims(
        &self,
        live_sessions: &[SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<Vec<(PendingRowId, SmSessionId)>, PendingStorageError> {
        // O(rows) lookup via a HashSet of `&SmSessionId` references —
        // avoids the O(rows × live_sessions) `Vec::contains` scan.
        // (Copilot review on PR #360.)
        let live: std::collections::HashSet<&SmSessionId> = live_sessions.iter().collect();
        let guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let stamps = self
            .claimed_at_ms
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut out = Vec::new();
        for queue in guard.values() {
            for row in queue.iter() {
                if let Some(session) = row.flushed_in_session.as_ref() {
                    // #1124 recency floor: a claim stamped after the
                    // floor is an in-flight flush (e.g. a `transient:`
                    // non-SM flush the live-set can never contain) —
                    // skip it. A missing stamp means "recency unknown"
                    // and is also skipped; the janitor adopts such
                    // claims via `stamp_unstamped_claims` first, so
                    // they age into eligibility instead of being
                    // released while possibly mid-flight.
                    let release_eligible = stamps
                        .get(&row.id)
                        .is_some_and(|claimed_at| *claimed_at <= claimed_before_ms);
                    if !live.contains(session) && release_eligible {
                        out.push((row.id.clone(), session.clone()));
                    }
                }
            }
        }
        Ok(out)
    }

    async fn stamp_unstamped_claims(&self, now_ms: i64) -> Result<u64, PendingStorageError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut stamps = self
            .claimed_at_ms
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut adopted = 0u64;
        for queue in guard.values() {
            for row in queue.iter() {
                if row.flushed_in_session.is_some() && !stamps.contains_key(&row.id) {
                    stamps.insert(row.id.clone(), now_ms);
                    adopted += 1;
                }
            }
        }
        Ok(adopted)
    }

    async fn count(&self, recipient: &BareJid) -> Result<u32, PendingStorageError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        Ok(guard.get(recipient).map(|q| q.len() as u32).unwrap_or(0))
    }

    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut removed = 0u64;
        let mut removed_ids = Vec::new();
        for queue in guard.values_mut() {
            let mut kept = VecDeque::with_capacity(queue.len());
            for row in queue.drain(..) {
                if row.original_receipt_at < cutoff {
                    removed_ids.push(row.id);
                    removed += 1;
                } else {
                    kept.push_back(row);
                }
            }
            *queue = kept;
        }
        guard.retain(|_, q| !q.is_empty());
        drop(guard);
        self.clear_notification_outboxed_markers(&removed_ids)?;
        self.clear_claimed_at(&removed_ids)?;
        Ok(removed)
    }

    async fn scrub_for_tombstone(
        &self,
        target: &crate::tombstone::TombstoneTarget,
    ) -> Result<u64, PendingStorageError> {
        Ok(self
            .scrub_for_tombstone_with_row_ids(target)
            .await?
            .removed_count)
    }

    async fn scrub_for_tombstone_with_row_ids(
        &self,
        target: &crate::tombstone::TombstoneTarget,
    ) -> Result<TombstoneScrubbedPendingRows, PendingStorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let mut removed_ids = Vec::new();
        for queue in guard.values_mut() {
            let mut kept = VecDeque::with_capacity(queue.len());
            for row in queue.drain(..) {
                if pending_row_matches_tombstone(&row, target) {
                    removed_ids.push(row.id);
                } else {
                    kept.push_back(row);
                }
            }
            *queue = kept;
        }
        guard.retain(|_, q| !q.is_empty());
        drop(guard);
        self.clear_notification_outboxed_markers(&removed_ids)?;
        self.clear_claimed_at(&removed_ids)?;
        Ok(TombstoneScrubbedPendingRows {
            removed_count: removed_ids.len() as u64,
            row_ids: removed_ids,
        })
    }
}

#[cfg(test)]
mod tests;
