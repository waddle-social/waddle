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
pub struct ObserverRetentionBatch {
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

const PUBLICATIONS_SQLITE: &str = "DELETE FROM extension_room_publications WHERE id IN (
    SELECT p.id FROM extension_room_publications p
    WHERE p.status IN ('published', 'stale') AND p.settled_at_ms <= ?
    ORDER BY p.settled_at_ms, p.id LIMIT ?)";
const PUBLICATIONS_POSTGRES: &str = "DELETE FROM extension_room_publications WHERE id IN (
    SELECT p.id FROM extension_room_publications p
    WHERE p.status IN ('published', 'stale') AND p.settled_at_ms <= ?
    ORDER BY p.settled_at_ms, p.id LIMIT ? FOR UPDATE OF p SKIP LOCKED)";

const WORK_SQLITE: &str = "DELETE FROM extension_room_observation_work WHERE id IN (
    SELECT w.id FROM extension_room_observation_work w
    WHERE w.status IN ('completed', 'terminal', 'stale') AND w.settled_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.work_id = w.id AND p.status = 'pending')
      AND NOT EXISTS (SELECT 1 FROM ingress_messages m
        WHERE m.message_key = w.message_key AND m.terminal_at IS NULL)
    ORDER BY w.settled_at_ms, w.id LIMIT ?)";
const WORK_POSTGRES: &str = "DELETE FROM extension_room_observation_work WHERE id IN (
    SELECT w.id FROM extension_room_observation_work w
    WHERE w.status IN ('completed', 'terminal', 'stale') AND w.settled_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.work_id = w.id AND p.status = 'pending')
      AND NOT EXISTS (SELECT 1 FROM ingress_messages m
        WHERE m.message_key = CAST(w.message_key AS UUID) AND m.terminal_at IS NULL)
    ORDER BY w.settled_at_ms, w.id LIMIT ? FOR UPDATE OF w SKIP LOCKED)";

const RECEIPTS_SQLITE: &str = "DELETE FROM extension_room_observation_receipts
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
const RECEIPTS_POSTGRES: &str = "DELETE FROM extension_room_observation_receipts
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

/// Revision mappings are drained before their source so an oversized chain
/// spends the batch budget instead of one unbounded statement.
const REVISIONS_SQLITE: &str = "DELETE FROM extension_room_source_revisions
  WHERE (room_jid, room_stanza_id) IN (
    SELECT v.room_jid, v.room_stanza_id
    FROM extension_room_source_revisions v
    JOIN extension_room_sources s ON s.source_key = v.source_key
    WHERE s.retracted = 1 AND s.captured_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.source_key = s.source_key)
    ORDER BY s.captured_at_ms, v.source_key, v.room_jid, v.room_stanza_id LIMIT ?)";
const REVISIONS_POSTGRES: &str = "DELETE FROM extension_room_source_revisions
  WHERE (room_jid, room_stanza_id) IN (
    SELECT v.room_jid, v.room_stanza_id
    FROM extension_room_source_revisions v
    JOIN extension_room_sources s ON s.source_key = v.source_key
    WHERE s.retracted = 1 AND s.captured_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.source_key = s.source_key)
    ORDER BY s.captured_at_ms, v.source_key, v.room_jid, v.room_stanza_id
    LIMIT ? FOR UPDATE OF v SKIP LOCKED)";

/// Lock first, then delete with the predicates re-evaluated by a fresh
/// statement: every writer that references a source (capture, correction,
/// claim, finish) holds this row lock while it does so.
const SOURCE_CANDIDATES_SQLITE: &str = "SELECT s.source_key FROM extension_room_sources s
    WHERE s.retracted = 1 AND s.captured_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_source_revisions v
        WHERE v.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.source_key = s.source_key)
    ORDER BY s.captured_at_ms, s.source_key LIMIT ?";
const SOURCE_CANDIDATES_POSTGRES: &str = "SELECT s.source_key FROM extension_room_sources s
    WHERE s.retracted = 1 AND s.captured_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_source_revisions v
        WHERE v.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.source_key = s.source_key)
    ORDER BY s.captured_at_ms, s.source_key LIMIT ? FOR UPDATE OF s SKIP LOCKED";
const SOURCE_DELETE: &str = "DELETE FROM extension_room_sources WHERE source_key IN (
    SELECT s.source_key FROM extension_room_sources s
    WHERE s.source_key = ? AND s.retracted = 1 AND s.captured_at_ms <= ?
      AND NOT EXISTS (SELECT 1 FROM extension_room_source_revisions v
        WHERE v.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_observation_work w
        WHERE w.source_key = s.source_key)
      AND NOT EXISTS (SELECT 1 FROM extension_room_publications p
        WHERE p.source_key = s.source_key))";

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
    remaining: u64,
) -> Result<u64, ObservationError> {
    if remaining == 0 {
        return Ok(0);
    }
    let limit = i64::try_from(remaining).map_err(|_| ObservationError::Codec)?;
    tx.execute(sql, crate::db_params![cutoff_ms, limit])
        .await
        .map_err(retention_error)
}

pub(super) async fn collect_expired(
    tx: &mut Transaction<'_>,
    now_ms: i64,
    limit: u32,
) -> Result<ObserverRetentionBatch, ObservationError> {
    let cutoff_ms = now_ms.saturating_sub(OBSERVER_HISTORY_RETENTION.num_milliseconds());
    let driver = tx.driver();
    let budget = u64::from(limit);
    let mut batch = ObserverRetentionBatch::default();
    // Publications first: a settled publication no longer protects its work.
    batch.publications = delete_batch(
        tx,
        dialect(driver, PUBLICATIONS_SQLITE, PUBLICATIONS_POSTGRES),
        cutoff_ms,
        budget,
    )
    .await?;
    batch.work = delete_batch(
        tx,
        dialect(driver, WORK_SQLITE, WORK_POSTGRES),
        cutoff_ms,
        budget.saturating_sub(batch.total()),
    )
    .await?;
    batch.receipts = delete_batch(
        tx,
        dialect(driver, RECEIPTS_SQLITE, RECEIPTS_POSTGRES),
        cutoff_ms,
        budget.saturating_sub(batch.total()),
    )
    .await?;
    batch.revisions = delete_batch(
        tx,
        dialect(driver, REVISIONS_SQLITE, REVISIONS_POSTGRES),
        cutoff_ms,
        budget.saturating_sub(batch.total()),
    )
    .await?;
    let remaining = budget.saturating_sub(batch.total());
    if remaining > 0 {
        let limit = i64::try_from(remaining).map_err(|_| ObservationError::Codec)?;
        let mut rows = tx
            .query(
                dialect(driver, SOURCE_CANDIDATES_SQLITE, SOURCE_CANDIDATES_POSTGRES),
                crate::db_params![cutoff_ms, limit],
            )
            .await
            .map_err(retention_error)?;
        let mut candidates = Vec::new();
        while let Some(row) = rows.next().await.map_err(retention_error)? {
            let key: String = row.get(0)?;
            candidates.push(key);
        }
        drop(rows);
        for key in candidates {
            batch.sources += tx
                .execute(SOURCE_DELETE, crate::db_params![&key, cutoff_ms])
                .await
                .map_err(retention_error)?;
        }
    }
    batch.exhausted = batch.total() >= budget;
    Ok(batch)
}
