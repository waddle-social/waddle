//! Recent-tombstone record for the promotion-time re-check (round-2
//! review R2).
//!
//! A XEP-0424/0425 retraction can race an in-flight XEP-0198 §5
//! promotion: the janitor's `drain_expired` has already moved the
//! session out of both in-memory maps into a local, the scrub finds
//! the stream nowhere (its pending row is not yet inserted either),
//! and the promotion then writes the retracted stanza into
//! `pending_delivery` AFTER the scrub ran — `confirm_drained` erases
//! the SM rows and the next login delivers retracted content.
//!
//! To close the window, `scrub_unacked_for_tombstone` records its
//! tombstone identity here and `promote_session_unacked` filters
//! matching stanzas out before running the promotion chain. Entries
//! are bounded by a generous TTL (promotion windows are seconds) plus
//! per-archive and global size caps.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tracing::warn;

use super::core::InMemorySmSessionRegistry;
use super::SmRegistryError;
use crate::tombstone::TombstoneTarget;

/// How long a recorded tombstone stays visible to the promotion-time
/// re-check. Promotion windows are seconds; ten minutes is generous.
pub const RECENT_TOMBSTONE_TTL: Duration = Duration::from_secs(600);

/// Hard cap on retained tombstone records — the global backstop.
/// Scrubs are rare (retraction / moderation only); when even the
/// per-archive cap can't keep the list under this bound, the oldest
/// record is evicted with a WARN (an unexpired foreign record is
/// being sacrificed).
pub const MAX_RECENT_TOMBSTONES: usize = 1024;

/// Per-archive-JID cap. Bounds how many records a single conversation
/// (one attacker retracting their own messages in a flood) can hold,
/// so one archive's flood evicts only ITS OWN oldest records and can
/// never flush another archive's unexpired record out of the
/// promotion-time re-check window.
pub const MAX_RECENT_TOMBSTONES_PER_ARCHIVE: usize = 64;

/// A recorded tombstone as seen by the promotion-time re-check: its
/// typed identity plus the wall-clock time the retraction was
/// recorded. Round-3 review finding 2: a tombstone applies BACKWARD in
/// time only — promotion treats a match as scrubbed only for stanzas
/// whose `original_receipt_at` predates `recorded_at_utc`, so a new
/// message that legitimately reuses a wire id in the same conversation
/// scope after the retraction is not silently lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentTombstoneRecord {
    pub key: TombstoneTarget,
    pub recorded_at_utc: DateTime<Utc>,
}

/// One recorded tombstone with its record times: the monotonic
/// `recorded_at` drives TTL eviction (never mix domains for expiry);
/// the wall-clock `recorded_at_utc` scopes the promotion-time match
/// backward in time against each stanza's `original_receipt_at`.
#[derive(Debug)]
pub(super) struct RecentTombstone {
    pub(super) key: TombstoneTarget,
    pub(super) recorded_at: Instant,
    pub(super) recorded_at_utc: DateTime<Utc>,
}

impl InMemorySmSessionRegistry {
    /// Record a tombstone identity for the promotion-time re-check.
    /// Called by `scrub_unacked_for_tombstone` before any scrub phase
    /// runs, so even a partially failed scrub leaves the record.
    pub(super) fn record_recent_tombstone(
        &self,
        target: &TombstoneTarget,
    ) -> Result<(), SmRegistryError> {
        self.record_recent_tombstone_at(target, Instant::now())
    }

    /// Test-visible variant taking an explicit record time so the TTL
    /// eviction is exercisable without wall-clock sleeps.
    ///
    /// Eviction policy (adversarial-review finding: an authenticated
    /// user retracting 1024+ of their own messages must not flush a
    /// victim's unexpired record):
    ///   1. expired entries go first (TTL sweep),
    ///   2. the inserting archive's own oldest entries go next
    ///      (per-archive cap), and only then
    ///   3. the global hard cap evicts the oldest entry overall,
    ///      warning that an unexpired foreign record was sacrificed.
    pub(super) fn record_recent_tombstone_at(
        &self,
        target: &TombstoneTarget,
        recorded_at: Instant,
    ) -> Result<(), SmRegistryError> {
        let mut recent = self
            .recent_tombstones
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Self::evict_stale_tombstones(&mut recent, Instant::now());
        // Per-archive cap: evict oldest entries WITHIN this archive
        // (the vec is push-ordered, so the first match is the oldest).
        let archive = target.archive_jid().clone();
        loop {
            let same_archive = recent
                .iter()
                .filter(|entry| entry.key.archive_jid() == &archive)
                .count();
            if same_archive < MAX_RECENT_TOMBSTONES_PER_ARCHIVE {
                break;
            }
            let Some(oldest_same_archive) = recent
                .iter()
                .position(|entry| entry.key.archive_jid() == &archive)
            else {
                break;
            };
            recent.remove(oldest_same_archive);
        }
        // Global backstop: everything left is unexpired and belongs to
        // other archives; evicting is lossy for their promotion-time
        // re-check, so surface it.
        while recent.len() >= MAX_RECENT_TOMBSTONES {
            let evicted = recent.remove(0);
            warn!(
                evicted_archive = %evicted.key.archive_jid(),
                inserting_archive = %archive,
                "recent-tombstone list at global cap: evicting an UNEXPIRED \
                 record for another archive; its promotion-time re-check \
                 window is truncated"
            );
        }
        recent.push(RecentTombstone {
            key: target.clone(),
            recorded_at,
            recorded_at_utc: Utc::now(),
        });
        Ok(())
    }

    /// Snapshot the tombstone records recorded within the TTL, oldest
    /// first. The Q6 promotion path consults this before inserting a
    /// drained session's unacked stanzas into `pending_delivery`, so a
    /// retraction that raced the drain still scrubs the in-flight copy
    /// — but only stanzas received before `recorded_at_utc`.
    pub fn recent_tombstones(&self) -> Result<Vec<RecentTombstoneRecord>, SmRegistryError> {
        let mut recent = self
            .recent_tombstones
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Self::evict_stale_tombstones(&mut recent, Instant::now());
        Ok(recent
            .iter()
            .map(|entry| RecentTombstoneRecord {
                key: entry.key.clone(),
                recorded_at_utc: entry.recorded_at_utc,
            })
            .collect())
    }

    fn evict_stale_tombstones(recent: &mut Vec<RecentTombstone>, now: Instant) {
        recent.retain(|entry| {
            now.checked_duration_since(entry.recorded_at)
                .is_none_or(|age| age <= RECENT_TOMBSTONE_TTL)
        });
    }
}
