use async_trait::async_trait;
use thiserror::Error;

use super::DetachedSession;
use crate::pending_delivery::SmSessionId;

/// Error type for SM session registry operations.
#[derive(Debug, Error)]
pub enum SmRegistryError {
    #[error("Session not found: {0}")]
    NotFound(String),

    #[error("Session expired")]
    Expired,

    #[error("Registry at capacity")]
    AtCapacity,

    #[error("Internal error: {0}")]
    Internal(String),
}

/// One exact SM replay entry deleted by a tombstone scrub.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TombstoneScrubbedSmEntry {
    pub stream_id: SmSessionId,
    pub sequence: u32,
}

/// Exact identities deleted by an SM tombstone scrub.
///
/// `removed_count` preserves the existing count-returning contract while
/// `entries` lets callers capture the exact replay rows removed when an
/// implementation can provide them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneScrubbedSmEntries {
    pub removed_count: usize,
    pub entries: Vec<TombstoneScrubbedSmEntry>,
}

impl TombstoneScrubbedSmEntries {
    pub fn count_only(removed_count: usize) -> Self {
        Self {
            removed_count,
            entries: Vec::new(),
        }
    }
}
/// Trait for SM session registries.
///
/// Implementations can be in-memory (for single-node) or distributed
/// (for clustered deployments).
#[async_trait]
pub trait SmSessionRegistry: Send + Sync {
    /// Store a detached session.
    ///
    /// The session can be retrieved later using `take_session` with the
    /// stream_id.
    ///
    /// Returns the sessions this store displaced from the in-memory
    /// pool — a superseded detached stream for the same full JID and/or
    /// the oldest session evicted on `max_sessions` overflow. Their
    /// unacked queues must NOT be discarded (XEP-0198 §5): the caller
    /// runs the promote → confirm chain on each and calls
    /// `confirm_drained` afterwards; durable rows for displaced
    /// sessions survive until that confirmation so a crash mid-
    /// promotion retries on the next startup.
    async fn store_session(
        &self,
        session: DetachedSession,
    ) -> Result<Vec<DetachedSession>, SmRegistryError>;

    /// Take (retrieve and remove) a session by stream ID.
    ///
    /// Returns the session if found and not expired, removing it from storage.
    /// This prevents the same session from being resumed twice.
    async fn take_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError>;

    /// Peek at a session without removing it.
    ///
    /// Useful for checking if a session exists before attempting resume.
    async fn peek_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError>;

    /// Clean up expired sessions.
    ///
    /// Returns the number of sessions removed.
    async fn cleanup_expired(&self) -> Result<usize, SmRegistryError>;

    /// Get the number of stored sessions.
    async fn session_count(&self) -> usize;

    /// Remove every unacked outbound `<message/>` stanza in stored
    /// sessions whose identity matches a XEP-0424 / XEP-0425 tombstone.
    /// Called when a tombstone is applied so a recipient mid-resume does
    /// not replay the pre-scrub stanza on the wire.
    ///
    /// `target` carries the typed tombstone identity
    /// ([`crate::tombstone::TombstoneTarget`]): groupchat/moderation
    /// scrubs match only the room-assigned XEP-0359 `<stanza-id
    /// by=room/>`; 1:1 retraction scrubs match the author's wire id
    /// only for messages FROM that author (plus the archive-stamped
    /// stanza-id branch). Both are scoped to the conversation archive,
    /// so a colliding client-chosen wire id from another sender or
    /// another chat can never be scrubbed.
    ///
    /// Returns the number of stanza entries removed across all stored
    /// sessions. Default impl is a no-op so registry implementations
    /// can opt in incrementally; the in-memory implementation
    /// overrides it.
    async fn scrub_unacked_for_tombstone(
        &self,
        _target: &crate::tombstone::TombstoneTarget,
    ) -> Result<usize, SmRegistryError> {
        Ok(0)
    }

    /// Typed sibling of [`Self::scrub_unacked_for_tombstone`] that returns
    /// exact `(stream, sequence)` identities when the implementation can
    /// provide them.
    ///
    /// Default impl preserves source compatibility for older backends by
    /// delegating to the existing count-only method and returning no entry
    /// identities.
    async fn scrub_unacked_for_tombstone_with_entries(
        &self,
        target: &crate::tombstone::TombstoneTarget,
    ) -> Result<TombstoneScrubbedSmEntries, SmRegistryError> {
        Ok(TombstoneScrubbedSmEntries::count_only(
            self.scrub_unacked_for_tombstone(target).await?,
        ))
    }
}

#[derive(Debug, Clone)]
pub enum SmClaimCompletion {
    Resumed(DetachedSession),
    Expired(DetachedSession),
    ReplayWindowTruncated(DetachedSession),
    HandledCountTooHigh(DetachedSession),
}
