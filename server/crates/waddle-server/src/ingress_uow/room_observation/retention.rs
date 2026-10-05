//! Bounded retention for settled room-observer history (#1901).
//!
//! Observer tables deliberately have no FK to canonical ingress rows, so they
//! cannot die with them the way `ingress_send_attempts` do. Each row instead
//! carries its own settlement clock and is collected once that clock passes
//! [`OBSERVER_HISTORY_RETENTION`] and nothing still depends on it:
//!
//! - active work (`pending`, `leased`, `started`) is never collected;
//! - a `pending` publication protects its work row and is never collected;
//! - work and receipts survive while their canonical `ingress_messages` row is
//!   non-terminal, because recovery may still rebuild the room-observer
//!   effect for that row and must find its evidence;
//! - only retracted, unreferenced sources are collected. A live source must
//!   keep resolving corrections for as long as the room archive holds the
//!   message, so its identity lives as long as the archive does.
//!
//! Every candidate is selected with its protective predicates in the same
//! statement. PostgreSQL skips rows another writer holds; SQLite runs the
//! batch under `BEGIN IMMEDIATE`.

use crate::db::{DatabaseDriver, DatabaseError, Transaction};

use super::ObservationError;

/// Same horizon as canonical retention (`ALIAS_RETENTION`), measured from the
/// observer row's own settlement rather than the canonical row's.
pub const OBSERVER_HISTORY_RETENTION: chrono::Duration = chrono::Duration::days(8);

/// Rows one [`collect_expired`] batch deleted, per table.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObserverRetentionBatch {
    pub publications: u64,
    pub work: u64,
    pub receipts: u64,
    /// Revision mappings of retracted sources.
    pub revisions: u64,
    pub sources: u64,
    /// The batch used its whole deletion budget, so more may be eligible.
    pub exhausted: bool,
}

impl ObserverRetentionBatch {
    pub fn total(&self) -> u64 {
        self.publications + self.work + self.receipts + self.revisions + self.sources
    }
}

pub(super) const PUBLICATIONS_SQLITE: &str = "DELETE FROM extension_room_publications WHERE id IN (
    SELECT p.id FROM extension_room_publications p
    WHERE p.status IN ('published', 'stale') AND p.settled_at_ms <= ?
    ORDER BY p.settled_at_ms, p.id LIMIT ?)";
pub(super) const PUBLICATIONS_POSTGRES: &str =
    "DELETE FROM extension_room_publications WHERE id IN (
    SELECT p.id FROM extension_room_publications p
    WHERE p.status IN ('published', 'stale') AND p.settled_at_ms <= ?
    ORDER BY p.settled_at_ms, p.id LIMIT ? FOR UPDATE OF p SKIP LOCKED)";

pub(super) const WORK_SQLITE: &str = "DELETE FROM extension_room_observation_work WHERE id IN (
    SELECT w.id FROM extension_room_observation_work w
    WHERE w.status IN ('completed', 'terminal', 'stale') AND w.settled_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.work_id = w.id AND p.status = 'pending')
      AND NOT EXISTS (SELECT 1 FROM ingress_messages m
        WHERE m.message_key = w.message_key AND m.terminal_at IS NULL)
    ORDER BY w.settled_at_ms, w.id LIMIT ?)";
pub(super) const WORK_POSTGRES: &str = "DELETE FROM extension_room_observation_work WHERE id IN (
    SELECT w.id FROM extension_room_observation_work w
    WHERE w.status IN ('completed', 'terminal', 'stale') AND w.settled_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.work_id = w.id AND p.status = 'pending')
      AND NOT EXISTS (SELECT 1 FROM ingress_messages m
        WHERE m.message_key = CAST(w.message_key AS UUID) AND m.terminal_at IS NULL)
    ORDER BY w.settled_at_ms, w.id LIMIT ? FOR UPDATE OF w SKIP LOCKED)";

pub(super) const RECEIPTS_SQLITE: &str = "DELETE FROM extension_room_observation_receipts
  WHERE (plugin_id, generation, room_jid, message_key) IN (
    SELECT r.plugin_id, r.generation, r.room_jid, r.message_key
    FROM extension_room_observation_receipts r
    WHERE r.recorded_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.plugin_id = r.plugin_id AND w.generation = r.generation
          AND w.room_jid = r.room_jid AND w.message_key = r.message_key
          AND w.status IN ('pending', 'leased', 'started'))
      AND NOT EXISTS (SELECT 1 FROM ingress_messages m
        WHERE m.message_key = r.message_key AND m.terminal_at IS NULL)
    ORDER BY r.recorded_at_ms, r.plugin_id, r.generation, r.room_jid, r.message_key LIMIT ?)";
pub(super) const RECEIPTS_POSTGRES: &str = "DELETE FROM extension_room_observation_receipts
  WHERE (plugin_id, generation, room_jid, message_key) IN (
    SELECT r.plugin_id, r.generation, r.room_jid, r.message_key
    FROM extension_room_observation_receipts r
    WHERE r.recorded_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.plugin_id = r.plugin_id AND w.generation = r.generation
          AND w.room_jid = r.room_jid AND w.message_key = r.message_key
          AND w.status IN ('pending', 'leased', 'started'))
      AND NOT EXISTS (SELECT 1 FROM ingress_messages m
        WHERE m.message_key = CAST(r.message_key AS UUID) AND m.terminal_at IS NULL)
    ORDER BY r.recorded_at_ms, r.plugin_id, r.generation, r.room_jid, r.message_key
    LIMIT ? FOR UPDATE OF r SKIP LOCKED)";

/// Retracted sources drained per batch. The row budget bounds the deleted
/// mappings; this bounds the `IN` list of the drain statement.
const REVISION_DRAIN_SOURCES: i64 = 16;

/// Revision mappings are drained before their source so an oversized chain
/// spends the batch budget instead of one unbounded statement. First lock a
/// few drainable retracted sources in `extension_room_sources_retracted_captured`
/// order. Every writer that adds a reference to a source (capture, correction,
/// claim, finish) holds that source's row lock while doing so, so the locked
/// sources stay unreferenced for the rest of this transaction and their
/// mappings can be deleted without re-checking each one.
pub(super) const DRAIN_CANDIDATES_SQLITE: &str = "SELECT s.source_key FROM extension_room_sources s
    WHERE s.retracted = 1 AND s.captured_at_ms <= ?
      AND EXISTS (SELECT 1 FROM extension_room_source_revisions v
        WHERE v.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.source_key = s.source_key)
    ORDER BY s.captured_at_ms, s.source_key LIMIT ?";
pub(super) const DRAIN_CANDIDATES_POSTGRES: &str =
    "SELECT s.source_key FROM extension_room_sources s
    WHERE s.retracted = 1 AND s.captured_at_ms <= ?
      AND EXISTS (SELECT 1 FROM extension_room_source_revisions v
        WHERE v.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.source_key = s.source_key)
    ORDER BY s.captured_at_ms, s.source_key LIMIT ? FOR UPDATE OF s SKIP LOCKED";

/// Delete up to the remaining budget of the locked sources' mappings, read in
/// `extension_room_source_revisions_source` order so no chain is sorted.
pub(super) fn revision_drain_sql(count: usize) -> String {
    let placeholders = vec!["?"; count].join(", ");
    format!(
        "DELETE FROM extension_room_source_revisions WHERE (room_jid, room_stanza_id) IN (
    SELECT v.room_jid, v.room_stanza_id FROM extension_room_source_revisions v
    WHERE v.source_key IN ({placeholders})
    ORDER BY v.source_key, v.room_jid, v.room_stanza_id LIMIT ?)"
    )
}

/// Lock first, then delete the whole locked set with one statement that
/// re-checks every predicate. Every writer that references a source (capture,
/// correction, claim, finish) holds this row lock while it does so, and the
/// delete is a fresh statement whose snapshot follows the lock, so it sees any
/// reference committed before the lock was granted. One statement per batch
/// keeps the transaction's round trips constant, whatever the candidate count.
pub(super) const SOURCE_CANDIDATES_SQLITE: &str =
    "SELECT s.source_key FROM extension_room_sources s
    WHERE s.retracted = 1 AND s.captured_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_source_revisions v
        WHERE v.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.source_key = s.source_key)
    ORDER BY s.captured_at_ms, s.source_key LIMIT ?";
pub(super) const SOURCE_CANDIDATES_POSTGRES: &str =
    "SELECT s.source_key FROM extension_room_sources s
    WHERE s.retracted = 1 AND s.captured_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_source_revisions v
        WHERE v.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.source_key = s.source_key)
    ORDER BY s.captured_at_ms, s.source_key LIMIT ? FOR UPDATE OF s SKIP LOCKED";
const SOURCE_DELETE_SUFFIX: &str = ") AND retracted = 1 AND captured_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_source_revisions v
        WHERE v.source_key = extension_room_sources.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.source_key = extension_room_sources.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.source_key = extension_room_sources.source_key)";

/// `DELETE` of `count` locked candidates; only the bound-parameter count
/// varies (at most the 256-row budget plus the cutoff, under SQLite's 999).
pub(super) fn source_delete_sql(count: usize) -> String {
    let placeholders = vec!["?"; count].join(", ");
    format!("DELETE FROM extension_room_sources WHERE source_key IN ({placeholders}{SOURCE_DELETE_SUFFIX}")
}

fn dialect(driver: DatabaseDriver, sqlite: &'static str, postgres: &'static str) -> &'static str {
    match driver {
        DatabaseDriver::Sqlite => sqlite,
        DatabaseDriver::Postgres => postgres,
    }
}

fn retention_error(error: DatabaseError) -> ObservationError {
    if crate::ingress_uow::is_database_timeout(&error) {
        ObservationError::Timeout
    } else {
        ObservationError::Database
    }
}

async fn delete_batch(
    tx: &mut Transaction<'_>,
    sql: &'static str,
    cutoff_ms: i64,
    remaining: u32,
) -> Result<u64, ObservationError> {
    if remaining == 0 {
        return Ok(0);
    }
    tx.execute(sql, crate::db_params![cutoff_ms, i64::from(remaining)])
        .await
        .map_err(retention_error)
}

/// Run a source-candidate `SELECT` (cutoff, limit) and collect its keys.
async fn locked_source_keys(
    tx: &mut Transaction<'_>,
    sql: &'static str,
    cutoff_ms: i64,
    limit: i64,
) -> Result<Vec<String>, ObservationError> {
    let mut rows = tx
        .query(sql, crate::db_params![cutoff_ms, limit])
        .await
        .map_err(retention_error)?;
    let mut keys = Vec::new();
    while let Some(row) = rows.next().await.map_err(retention_error)? {
        let key: String = row.get(0)?;
        keys.push(key);
    }
    Ok(keys)
}

pub(super) async fn collect_expired(
    tx: &mut Transaction<'_>,
    now_ms: i64,
    limit: u32,
) -> Result<ObserverRetentionBatch, ObservationError> {
    let cutoff_ms = now_ms.saturating_sub(OBSERVER_HISTORY_RETENTION.num_milliseconds());
    let driver = tx.driver();
    let mut batch = ObserverRetentionBatch::default();
    let remaining = |batch: &ObserverRetentionBatch| {
        u32::try_from(u64::from(limit).saturating_sub(batch.total())).unwrap_or(0)
    };
    // Publications first: a settled publication no longer protects its work.
    batch.publications = delete_batch(
        tx,
        dialect(driver, PUBLICATIONS_SQLITE, PUBLICATIONS_POSTGRES),
        cutoff_ms,
        limit,
    )
    .await?;
    batch.work = delete_batch(
        tx,
        dialect(driver, WORK_SQLITE, WORK_POSTGRES),
        cutoff_ms,
        remaining(&batch),
    )
    .await?;
    batch.receipts = delete_batch(
        tx,
        dialect(driver, RECEIPTS_SQLITE, RECEIPTS_POSTGRES),
        cutoff_ms,
        remaining(&batch),
    )
    .await?;
    let revision_budget = remaining(&batch);
    // The drain caps how many sources one batch touches, independently of the
    // row budget. Reaching that cap means more drainable sources may remain,
    // so the batch must report itself exhausted even with budget to spare.
    let mut drain_cap_reached = false;
    if revision_budget > 0 {
        let drained = locked_source_keys(
            tx,
            dialect(driver, DRAIN_CANDIDATES_SQLITE, DRAIN_CANDIDATES_POSTGRES),
            cutoff_ms,
            REVISION_DRAIN_SOURCES,
        )
        .await?;
        drain_cap_reached = drained.len() as i64 >= REVISION_DRAIN_SOURCES;
        if !drained.is_empty() {
            let sql = revision_drain_sql(drained.len());
            let mut params: Vec<crate::db::Value> =
                drained.into_iter().map(crate::db::Value::from).collect();
            params.push(crate::db::Value::from(i64::from(revision_budget)));
            batch.revisions = tx.execute(&sql, params).await.map_err(retention_error)?;
        }
    }
    let remaining = remaining(&batch);
    if remaining > 0 {
        let candidates = locked_source_keys(
            tx,
            dialect(driver, SOURCE_CANDIDATES_SQLITE, SOURCE_CANDIDATES_POSTGRES),
            cutoff_ms,
            i64::from(remaining),
        )
        .await?;
        if !candidates.is_empty() {
            let sql = source_delete_sql(candidates.len());
            let mut params: Vec<crate::db::Value> =
                candidates.into_iter().map(crate::db::Value::from).collect();
            params.push(crate::db::Value::from(cutoff_ms));
            batch.sources = tx.execute(&sql, params).await.map_err(retention_error)?;
        }
    }
    batch.exhausted = batch.total() >= u64::from(limit) || drain_cap_reached;
    Ok(batch)
}
