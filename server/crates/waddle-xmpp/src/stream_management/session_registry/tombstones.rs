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
    pub archive_jid: String,
}

/// One recorded tombstone with its record time, for TTL eviction.
#[derive(Debug)]
pub(super) struct RecentTombstone {
    pub(super) key: TombstoneKey,
    pub(super) recorded_at: Instant,
}

impl InMemorySmSessionRegistry {
    /// Record a tombstone identity for the promotion-time re-check.
    /// Called by `scrub_unacked_for_tombstone` before any scrub phase
    /// runs, so even a partially failed scrub leaves the record.
    pub(super) fn record_recent_tombstone(
        &self,
        target_id: &str,
        archive_jid: &str,
    ) -> Result<(), SmRegistryError> {
        self.record_recent_tombstone_at(target_id, archive_jid, Instant::now())
    }

    /// Test-visible variant taking an explicit record time so the TTL
    /// eviction is exercisable without wall-clock sleeps.
    pub(super) fn record_recent_tombstone_at(
        &self,
        target_id: &str,
        archive_jid: &str,
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
                archive_jid: archive_jid.to_string(),
            },
            recorded_at,
        });
        Ok(())
    }

    /// Snapshot the tombstone identities recorded within the TTL,
    /// oldest first. The Q6 promotion path consults this before
    /// inserting a drained session's unacked stanzas into
    /// `pending_delivery`, so a retraction that raced the drain still
    /// scrubs the in-flight copy.
    pub fn recent_tombstones(&self) -> Result<Vec<TombstoneKey>, SmRegistryError> {
        let mut recent = self
            .recent_tombstones
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Self::evict_stale_tombstones(&mut recent, Instant::now());
        Ok(recent.iter().map(|entry| entry.key.clone()).collect())
    }

    fn evict_stale_tombstones(recent: &mut Vec<RecentTombstone>, now: Instant) {
        recent.retain(|entry| {
            now.checked_duration_since(entry.recorded_at)
                .is_none_or(|age| age <= RECENT_TOMBSTONE_TTL)
        });
    }
}
