use async_trait::async_trait;
use thiserror::Error;

use super::DetachedSession;

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
/// Trait for SM session registries.
///
/// Implementations can be in-memory (for single-node) or distributed
/// (for clustered deployments).
#[async_trait]
pub trait SmSessionRegistry: Send + Sync {
    /// Store a detached session.
    ///
    /// The session can be retrieved later using `take_session` with the stream_id.
    async fn store_session(&self, session: DetachedSession) -> Result<(), SmRegistryError>;

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
    /// `target_id` matches either the cached message's wire `id`
    /// attribute (typical for 1:1 retractions targeting the original
    /// message id) **or** any XEP-0359 `<stanza-id id='…'/>` child
    /// (typical for groupchat retractions that key by the room's
    /// stanza-id per the "archive id == wire stanza-id" invariant).
    ///
    /// `archive_jid` scopes the match to a specific conversation: a
    /// cached message is only removed if its `from` or `to` bare-equals
    /// `archive_jid`. This prevents cross-conversation collateral
    /// damage when two clients independently reuse a short message id
    /// in different chats — without scoping, retracting "msg-1" in one
    /// chat would silently delete unrelated "msg-1" stanzas queued for
    /// other recipients.
    ///
    /// Returns the number of stanza entries removed across all stored
    /// sessions. Default impl is a no-op so registry implementations
    /// can opt in incrementally; the in-memory implementation
    /// overrides it.
    async fn scrub_unacked_for_tombstone(
        &self,
        _target_id: &str,
        _archive_jid: &str,
    ) -> Result<usize, SmRegistryError> {
        Ok(0)
    }
}

#[derive(Debug, Clone)]
pub enum SmClaimCompletion {
    Resumed(DetachedSession),
    Expired(DetachedSession),
    ReplayWindowTruncated(DetachedSession),
}
