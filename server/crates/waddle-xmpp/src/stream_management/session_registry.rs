//! Session Registry for XEP-0198 Stream Management
//!
//! This module provides server-side storage for detached stream sessions,
//! allowing streams to be resumed after disconnection.
//!
//! When a client disconnects with SM enabled and resumption requested,
//! the server stores the session state. When the client reconnects with
//! a resume request, the server can restore the session.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jid::FullJid;
use thiserror::Error;
use tracing::debug;

/// Default session timeout (5 minutes)
pub const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 300;

/// Maximum number of sessions to store
pub const DEFAULT_MAX_SESSIONS: usize = 10000;

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

/// A detached stream management session.
///
/// Contains all the state needed to resume a stream after disconnection.
#[derive(Debug, Clone)]
pub struct DetachedSession {
    /// The unique stream ID
    pub stream_id: String,
    /// The full JID of the session owner
    pub jid: FullJid,
    /// Server's inbound stanza count at detach time
    pub inbound_count: u32,
    /// Server's outbound stanza count at detach time
    pub outbound_count: u32,
    /// Last acknowledged outbound stanza count
    pub last_acked: u32,
    /// Unacknowledged stanzas (sequence, xml)
    pub unacked_stanzas: Vec<(u32, String)>,
    /// Maximum resumption time in seconds
    pub max_resume_time: Option<u32>,
    /// When the session was detached
    pub detached_at: Instant,
}

impl DetachedSession {
    /// Check if the session has expired.
    pub fn is_expired(&self) -> bool {
        let max_time = self
            .max_resume_time
            .unwrap_or(DEFAULT_SESSION_TIMEOUT_SECS as u32);
        self.detached_at.elapsed() > Duration::from_secs(max_time as u64)
    }

    /// Get remaining time until expiration.
    pub fn remaining_time(&self) -> Duration {
        let max_time = Duration::from_secs(
            self.max_resume_time
                .unwrap_or(DEFAULT_SESSION_TIMEOUT_SECS as u32) as u64,
        );
        max_time.saturating_sub(self.detached_at.elapsed())
    }

    /// Get the number of stanzas that would need to be resent.
    ///
    /// `client_h` is what the client reports as last received.
    pub fn stanzas_to_resend_count(&self, client_h: u32) -> usize {
        self.unacked_stanzas
            .iter()
            .filter(|(seq, _)| sequence_gt(*seq, client_h))
            .count()
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
}

/// In-memory implementation of the SM session registry.
///
/// Suitable for single-node deployments. For clustered deployments,
/// use a distributed implementation backed by Redis or similar.
#[derive(Debug)]
pub struct InMemorySmSessionRegistry {
    sessions: RwLock<HashMap<String, DetachedSession>>,
    max_sessions: usize,
}

impl Default for InMemorySmSessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySmSessionRegistry {
    /// Create a new in-memory registry with default settings.
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            max_sessions: DEFAULT_MAX_SESSIONS,
        }
    }

    /// Create a registry with custom settings.
    pub fn with_capacity(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::with_capacity(max_sessions.min(10000))),
            max_sessions,
        }
    }
}

#[async_trait]
impl SmSessionRegistry for InMemorySmSessionRegistry {
    async fn store_session(&self, session: DetachedSession) -> Result<(), SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        // Clean up expired sessions if at capacity
        if sessions.len() >= self.max_sessions {
            cleanup_expired_internal(&mut sessions);
        }

        // Still at capacity after cleanup?
        if sessions.len() >= self.max_sessions {
            // Remove oldest session
            if let Some(oldest_key) = sessions
                .iter()
                .min_by_key(|(_, s)| s.detached_at)
                .map(|(k, _)| k.clone())
            {
                sessions.remove(&oldest_key);
                debug!(stream_id = %oldest_key, "Evicted oldest SM session to make room");
            }
        }

        let stream_id = session.stream_id.clone();
        sessions.insert(stream_id.clone(), session);

        debug!(stream_id = %stream_id, count = sessions.len(), "Stored detached SM session");
        Ok(())
    }

    async fn take_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        match sessions.remove(stream_id) {
            Some(session) => {
                if session.is_expired() {
                    debug!(stream_id = %stream_id, "SM session found but expired");
                    Ok(None)
                } else {
                    debug!(stream_id = %stream_id, "Retrieved and removed SM session");
                    Ok(Some(session))
                }
            }
            None => {
                debug!(stream_id = %stream_id, "SM session not found");
                Ok(None)
            }
        }
    }

    async fn peek_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        match sessions.get(stream_id) {
            Some(session) => {
                if session.is_expired() {
                    Ok(None)
                } else {
                    Ok(Some(session.clone()))
                }
            }
            None => Ok(None),
        }
    }

    async fn cleanup_expired(&self) -> Result<usize, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let removed = cleanup_expired_internal(&mut sessions);
        Ok(removed)
    }

    async fn session_count(&self) -> usize {
        self.sessions.read().map(|s| s.len()).unwrap_or(0)
    }
}

/// Internal helper to clean up expired sessions (requires write lock already held).
fn cleanup_expired_internal(sessions: &mut HashMap<String, DetachedSession>) -> usize {
    let initial_count = sessions.len();
    sessions.retain(|_, s| !s.is_expired());
    let removed = initial_count - sessions.len();

    if removed > 0 {
        debug!(
            removed = removed,
            remaining = sessions.len(),
            "Cleaned up expired SM sessions"
        );
    }

    removed
}

/// Check if sequence a > b, handling wrap-around.
fn sequence_gt(a: u32, b: u32) -> bool {
    if a == b {
        return false;
    }
    let diff = a.wrapping_sub(b);
    diff < 0x8000_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_jid() -> FullJid {
        "user@example.com/resource".parse().unwrap()
    }

    fn make_test_session(stream_id: &str) -> DetachedSession {
        DetachedSession {
            stream_id: stream_id.to_string(),
            jid: make_test_jid(),
            inbound_count: 10,
            outbound_count: 15,
            last_acked: 12,
            unacked_stanzas: vec![
                (13, "<msg1/>".to_string()),
                (14, "<msg2/>".to_string()),
                (15, "<msg3/>".to_string()),
            ],
            max_resume_time: Some(300),
            detached_at: Instant::now(),
        }
    }

    #[tokio::test]
    async fn test_store_and_take_session() {
        let registry = InMemorySmSessionRegistry::new();

        let session = make_test_session("stream-123");
        registry.store_session(session).await.unwrap();

        assert_eq!(registry.session_count().await, 1);

        // Take the session
        let retrieved = registry.take_session("stream-123").await.unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.stream_id, "stream-123");
        assert_eq!(retrieved.outbound_count, 15);

        // Session should be gone now
        assert_eq!(registry.session_count().await, 0);
        let again = registry.take_session("stream-123").await.unwrap();
        assert!(again.is_none());
    }

    #[tokio::test]
    async fn test_peek_session() {
        let registry = InMemorySmSessionRegistry::new();

        let session = make_test_session("stream-456");
        registry.store_session(session).await.unwrap();

        // Peek should not remove
        let peeked = registry.peek_session("stream-456").await.unwrap();
        assert!(peeked.is_some());
        assert_eq!(registry.session_count().await, 1);

        // Peek again
        let peeked2 = registry.peek_session("stream-456").await.unwrap();
        assert!(peeked2.is_some());
    }

    #[tokio::test]
    async fn test_session_not_found() {
        let registry = InMemorySmSessionRegistry::new();

        let result = registry.take_session("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_session_expired() {
        let registry = InMemorySmSessionRegistry::new();

        // Create an already-expired session
        let mut session = make_test_session("stream-expired");
        session.max_resume_time = Some(0); // 0 seconds means expired immediately

        registry.store_session(session).await.unwrap();

        // Wait a tiny bit to ensure expiration
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Should return None because expired
        let result = registry.take_session("stream-expired").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_cleanup_expired() {
        let registry = InMemorySmSessionRegistry::new();

        // Store some sessions
        let mut expired = make_test_session("stream-exp1");
        expired.max_resume_time = Some(0);
        registry.store_session(expired).await.unwrap();

        let valid = make_test_session("stream-valid");
        registry.store_session(valid).await.unwrap();

        // Wait for expiration
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Cleanup
        let removed = registry.cleanup_expired().await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(registry.session_count().await, 1);

        // Valid session should still be there
        let result = registry.take_session("stream-valid").await.unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_capacity_limit() {
        let registry = InMemorySmSessionRegistry::with_capacity(3);

        // Store 3 sessions
        for i in 0..3 {
            let session = make_test_session(&format!("stream-{}", i));
            registry.store_session(session).await.unwrap();
        }

        assert_eq!(registry.session_count().await, 3);

        // Store a 4th - should evict oldest
        let session = make_test_session("stream-new");
        registry.store_session(session).await.unwrap();

        assert_eq!(registry.session_count().await, 3);

        // stream-0 should be gone (oldest)
        let result = registry.take_session("stream-0").await.unwrap();
        assert!(result.is_none());

        // stream-new should be there
        let result = registry.take_session("stream-new").await.unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_stanzas_to_resend_count() {
        let session = make_test_session("test");

        // Client says h=12, we have 13, 14, 15 - all 3 need resending
        assert_eq!(session.stanzas_to_resend_count(12), 3);

        // Client says h=14, we have 13, 14, 15 - only 15 needs resending
        assert_eq!(session.stanzas_to_resend_count(14), 1);

        // Client says h=15, we have 13, 14, 15 - none need resending
        assert_eq!(session.stanzas_to_resend_count(15), 0);
    }

    #[test]
    fn test_remaining_time() {
        let session = make_test_session("test");

        let remaining = session.remaining_time();
        assert!(remaining.as_secs() <= 300);
        assert!(remaining.as_secs() >= 299); // Should be close to 300
    }
}
