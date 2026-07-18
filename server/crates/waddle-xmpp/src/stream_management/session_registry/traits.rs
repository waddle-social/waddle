use async_trait::async_trait;
use thiserror::Error;

use super::{DetachedReplaySequenceConflict, DetachedSession};

/// Error type for SM session registry operations.
#[derive(Debug, Error)]
pub enum SmRegistryError {
    #[error("Session not found: {0}")]
    NotFound(String),

    #[error("Session expired")]
    Expired,

    #[error("Registry at capacity")]
    AtCapacity,

    #[error(transparent)]
    DetachedReplaySequenceConflict(#[from] DetachedReplaySequenceConflict),

    /// A generation-scoped lease was valid when acquired but was revoked
    /// before a protected persistence mutation began.
    #[error("SM promotion authority was revoked")]
    PromotionAuthorityLost,

    #[error("Internal error: {0}")]
    Internal(String),

    /// The persistence acknowledgement was ambiguous, but the registry kept
    /// the detached successor published and claim-backed because the write
    /// may have committed. Callers must complete normal detached-session
    /// sidecar cleanup rather than treating this as a terminal detach loss.
    #[error("Detached-session resumability preserved after ambiguous persistence outcome: {0}")]
    ResumabilityPreserved(#[source] super::super::persistence::SmPersistenceError),

    /// The durable snapshot may exist and the ownership CAS may have
    /// committed, but its result was not observed. The registry retains the
    /// local successor and bounded reconciliation responsibility, while a
    /// force-detach caller must re-check instead of treating the stream as
    /// proven stealable.
    #[error("Detached-session claim acquisition is ambiguous and tracked for reconciliation")]
    DetachClaimAmbiguous,

    /// The ownership backend definitively refused the detach claim. Local
    /// resumability has been removed, while the full detached payload remains
    /// queued as a non-authoritative promotion carrier.
    #[error("Detached-session claim acquisition was rejected")]
    DetachClaimRejected,

    #[error(transparent)]
    Persistence(#[from] super::super::persistence::SmPersistenceError),
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
}

#[derive(Debug, Clone)]
pub enum SmClaimCompletion {
    Resumed(DetachedSession),
    Expired(DetachedSession),
    ReplayWindowTruncated(DetachedSession),
    HandledCountTooHigh(DetachedSession),
}
