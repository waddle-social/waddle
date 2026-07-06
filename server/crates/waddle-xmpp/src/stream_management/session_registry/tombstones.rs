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
//! a hard size cap.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use super::core::InMemorySmSessionRegistry;
use super::SmRegistryError;

/// How long a recorded tombstone stays visible to the promotion-time
/// re-check. Promotion windows are seconds; ten minutes is generous.
pub const RECENT_TOMBSTONE_TTL: Duration = Duration::from_secs(600);

/// Hard cap on retained tombstone records. Scrubs are rare
/// (retraction / moderation only); overflow evicts the oldest.
pub const MAX_RECENT_TOMBSTONES: usize = 1024;

/// Identity of a recently applied XEP-0424/0425 tombstone: the target
/// message id plus the conversation (archive) scope, matching the two
/// inputs of [`crate::tombstone::message_element_matches_tombstone`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneKey {
    pub target_id: String,
    pub archive_jid: jid::BareJid,
}

/// A recorded tombstone as seen by the promotion-time re-check: its
/// identity plus the wall-clock time the retraction was recorded.
/// Round-3 review finding 2: a tombstone applies BACKWARD in time
/// only — promotion treats a match as scrubbed only for stanzas whose
/// `original_receipt_at` predates `recorded_at_utc`, so a new message
/// that legitimately reuses a wire id in the same conversation scope
/// after the retraction is not silently lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentTombstoneRecord {
    pub key: TombstoneKey,
    pub recorded_at_utc: DateTime<Utc>,
}

/// One recorded tombstone with its record times: the monotonic
/// `recorded_at` drives TTL eviction (never mix domains for expiry);
/// the wall-clock `recorded_at_utc` scopes the promotion-time match
/// backward in time against each stanza's `original_receipt_at`.
#[derive(Debug)]
pub(super) struct RecentTombstone {
    pub(super) key: TombstoneKey,
    pub(super) recorded_at: Instant,
    pub(super) recorded_at_utc: DateTime<Utc>,
}

impl InMemorySmSessionRegistry {
    /// Record a tombstone identity for the promotion-time re-check.
    /// Called by `scrub_unacked_for_tombstone` before any scrub phase
    /// runs, so even a partially failed scrub leaves the record.
    pub(super) fn record_recent_tombstone(
        &self,
        target_id: &str,
        archive_jid: &jid::BareJid,
    ) -> Result<(), SmRegistryError> {
        self.record_recent_tombstone_at(target_id, archive_jid, Instant::now())
    }

    /// Test-visible variant taking an explicit record time so the TTL
    /// eviction is exercisable without wall-clock sleeps.
    pub(super) fn record_recent_tombstone_at(
        &self,
        target_id: &str,
        archive_jid: &jid::BareJid,
        recorded_at: Instant,
    ) -> Result<(), SmRegistryError> {
        let mut recent = self
            .recent_tombstones
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Self::evict_stale_tombstones(&mut recent, Instant::now());
        if recent.len() >= MAX_RECENT_TOMBSTONES {
            let overflow = recent.len() + 1 - MAX_RECENT_TOMBSTONES;
            recent.drain(..overflow);
        }
        recent.push(RecentTombstone {
            key: TombstoneKey {
                target_id: target_id.to_string(),
                archive_jid: archive_jid.clone(),
            },
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
