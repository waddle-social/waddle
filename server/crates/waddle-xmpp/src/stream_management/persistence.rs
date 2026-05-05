//! XEP-0198 stream-management durable persistence (issue #209, slice
//! (d), locked Q8 = B).
//!
//! The legacy [`super::session_registry::InMemorySmSessionRegistry`]
//! holds detached sessions and their unacked queues purely in memory:
//! a server crash loses every in-flight session and every unacked
//! stanza. Locked Q8 = B requires sessions and unacked queues to
//! survive a restart so SM resumption (XEP-0198 §5) works after crash
//! and so XEP-0198 §5 line 364 ("treat unacked as if never received")
//! has a durable home for promotion into `pending_delivery`.
//!
//! This module defines the typed persistence contract — the trait and
//! the typed value records — separated from the in-memory registry
//! so the migration can land incrementally:
//!
//! 1. **This commit** — trait + record types + `InMemoryPersistence`
//!    fake (no behavior change to the live system; structural
//!    groundwork only).
//! 2. **Follow-up** — `DatabaseSmPersistence` libSQL/Postgres impl in
//!    `waddle-server`, schema migration with `sm_sessions` and
//!    `sm_unacked` tables.
//! 3. **Follow-up** — `InMemorySmSessionRegistry::store_session` /
//!    `record_detached_outbound` etc. wired through the persistence
//!    storage so writes persist immediately. Restart restores the
//!    in-memory view from the persistence layer.
//! 4. **Follow-up** — graceful-shutdown drain that walks every live
//!    session, runs each unacked stanza through `classify_dm_intake`
//!    and `pending_delivery` insertion (Q6 promotion).

use std::time::Duration;

use async_trait::async_trait;
use jid::FullJid;
use thiserror::Error;
use xmpp_parsers::presence::Show;

/// Errors returned by [`SmPersistenceStorage`] implementations.
#[derive(Debug, Error)]
pub enum SmPersistenceError {
    #[error("SM persistence error: {0}")]
    Other(String),
}

/// A durable record of an XEP-0198 detached session.
///
/// Mirrors the fields on
/// [`super::session_registry::DetachedSession`] that need to survive a
/// process restart. Notable difference: timestamps are typed as
/// [`chrono::DateTime<chrono::Utc>`] rather than [`std::time::Instant`]
/// — `Instant` is process-relative and cannot be persisted.
///
/// `unacked_stanzas` carries the wire XML for each unacked stanza,
/// keyed by its server-side outbound sequence. Locked Q6c requires
/// each stanza to retain its **original receipt time** (so promoted
/// rows stamp `<delay/>` per XEP-0203 §4.1 + XEP-0198 §5 line 364);
/// the `original_receipt_at` field on
/// [`PersistedUnackedStanza`] holds that value.
#[derive(Debug, Clone)]
pub struct PersistedSession {
    pub stream_id: String,
    pub user_id: String,
    pub jid: FullJid,
    pub inbound_count: u32,
    pub outbound_count: u32,
    pub last_acked: u32,
    pub max_resume_time: Option<u32>,
    pub detached_at: chrono::DateTime<chrono::Utc>,
    pub max_resume_duration: Duration,
    pub carbons_enabled: bool,
    pub roster_interested: bool,
    pub presence_available: bool,
    pub presence_show: Option<Show>,
    pub presence_status: Option<String>,
    pub presence_priority: i8,
}

/// One row in the `sm_unacked` table — a stanza sent to a session
/// that has not yet been acknowledged.
#[derive(Debug, Clone)]
pub struct PersistedUnackedStanza {
    /// XEP-0198 stream-id of the owning session.
    pub stream_id: String,
    /// Server-side outbound sequence number; ordered ascending.
    pub sequence: u32,
    /// Stanza wire XML (serialized minidom). Stored as the original
    /// outbound payload so SM resumption replays bit-identically.
    pub stanza_xml: String,
    /// Original receipt time at the server. Carried so that the Q6
    /// promotion path can stamp the `<delay/>` per XEP-0203 §4.1 +
    /// XEP-0198 §5 line 364 with the correct timestamp.
    pub original_receipt_at: chrono::DateTime<chrono::Utc>,
}

/// Persistent storage contract for XEP-0198 SM session state.
///
/// Operations are async to allow libSQL / Postgres backends; the
/// in-memory implementation in this module is fully synchronous
/// internally but exposes the same async trait so handlers can call
/// it without conditionalizing on backend.
#[async_trait]
pub trait SmPersistenceStorage: Send + Sync {
    /// Insert or replace a session record. Called when a stream
    /// detaches and the session is moved into the resumable pool.
    async fn upsert_session(&self, session: PersistedSession) -> Result<(), SmPersistenceError>;

    /// Look up a session by stream-id. Used on `<resume/>` to verify
    /// the previd is recognized and to rebuild the in-memory session.
    async fn get_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<PersistedSession>, SmPersistenceError>;

    /// Delete a session and all its unacked stanzas. Called on
    /// successful `<resumed/>` (the session is now live again, no
    /// longer detached) and on session timeout.
    async fn delete_session(&self, stream_id: &str) -> Result<(), SmPersistenceError>;

    /// Append an outbound stanza to the unacked queue for the named
    /// session.
    async fn append_unacked(
        &self,
        stanza: PersistedUnackedStanza,
    ) -> Result<(), SmPersistenceError>;

    /// Remove unacked entries up to and including `up_to_sequence`.
    /// Called when an `<a h='N'/>` ack arrives.
    async fn ack_through(
        &self,
        stream_id: &str,
        up_to_sequence: u32,
    ) -> Result<u64, SmPersistenceError>;

    /// Read every unacked stanza for a session in sequence order.
    /// Used by `<resumed/>` to replay and by the Q6 promotion path to
    /// drain on session expiry.
    async fn list_unacked(
        &self,
        stream_id: &str,
    ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError>;

    /// Enumerate every persisted session whose `detached_at +
    /// max_resume_duration` is in the past. Used by the janitor that
    /// expires timed-out sessions and by the graceful-shutdown drain.
    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PersistedSession>, SmPersistenceError>;
}

/// In-memory implementation suitable for tests and as the structural
/// fake that future libSQL/Postgres impls drop in for.
#[derive(Debug, Default)]
pub struct InMemorySmPersistence {
    inner: std::sync::Mutex<InMemoryState>,
}

#[derive(Debug, Default)]
struct InMemoryState {
    sessions: std::collections::HashMap<String, PersistedSession>,
    // Per-session unacked queue keyed by stream_id.
    unacked: std::collections::HashMap<String, Vec<PersistedUnackedStanza>>,
}

impl InMemorySmPersistence {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SmPersistenceStorage for InMemorySmPersistence {
    async fn upsert_session(&self, session: PersistedSession) -> Result<(), SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        guard.sessions.insert(session.stream_id.clone(), session);
        Ok(())
    }

    async fn get_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<PersistedSession>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(guard.sessions.get(stream_id).cloned())
    }

    async fn delete_session(&self, stream_id: &str) -> Result<(), SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        guard.sessions.remove(stream_id);
        guard.unacked.remove(stream_id);
        Ok(())
    }

    async fn append_unacked(
        &self,
        stanza: PersistedUnackedStanza,
    ) -> Result<(), SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        guard
            .unacked
            .entry(stanza.stream_id.clone())
            .or_default()
            .push(stanza);
        Ok(())
    }

    async fn ack_through(
        &self,
        stream_id: &str,
        up_to_sequence: u32,
    ) -> Result<u64, SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let queue = match guard.unacked.get_mut(stream_id) {
            Some(q) => q,
            None => return Ok(0),
        };
        let before = queue.len();
        queue.retain(|s| s.sequence > up_to_sequence);
        Ok((before - queue.len()) as u64)
    }

    async fn list_unacked(
        &self,
        stream_id: &str,
    ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let mut rows = guard.unacked.get(stream_id).cloned().unwrap_or_default();
        rows.sort_by_key(|s| s.sequence);
        Ok(rows)
    }

    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let mut out = Vec::new();
        for session in guard.sessions.values() {
            let expires_at = session.detached_at
                + chrono::Duration::from_std(session.max_resume_duration)
                    .unwrap_or(chrono::Duration::seconds(0));
            if expires_at <= now {
                out.push(session.clone());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn full(s: &str) -> FullJid {
        s.parse().unwrap()
    }

    fn fixture_session(stream_id: &str) -> PersistedSession {
        PersistedSession {
            stream_id: stream_id.to_string(),
            user_id: "alice".to_string(),
            jid: full("alice@example.com/web"),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            max_resume_time: Some(60),
            detached_at: Utc::now(),
            max_resume_duration: Duration::from_secs(60),
            carbons_enabled: true,
            roster_interested: true,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 1,
        }
    }

    fn fixture_unacked(stream_id: &str, sequence: u32) -> PersistedUnackedStanza {
        PersistedUnackedStanza {
            stream_id: stream_id.to_string(),
            sequence,
            stanza_xml: format!(
                "<message xmlns='jabber:client'><body>m{sequence}</body></message>"
            ),
            original_receipt_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn upsert_get_round_trip() {
        let store = InMemorySmPersistence::new();
        let s = fixture_session("stream-1");
        store.upsert_session(s.clone()).await.unwrap();
        let loaded = store.get_session("stream-1").await.unwrap().unwrap();
        assert_eq!(loaded.user_id, s.user_id);
        assert_eq!(loaded.carbons_enabled, true);
    }

    #[tokio::test]
    async fn ack_through_drops_only_acked_sequences() {
        let store = InMemorySmPersistence::new();
        for seq in 1..=4 {
            store
                .append_unacked(fixture_unacked("stream-1", seq))
                .await
                .unwrap();
        }
        let dropped = store.ack_through("stream-1", 2).await.unwrap();
        assert_eq!(dropped, 2);
        let remaining = store.list_unacked("stream-1").await.unwrap();
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].sequence, 3);
        assert_eq!(remaining[1].sequence, 4);
    }

    #[tokio::test]
    async fn delete_session_clears_unacked_too() {
        let store = InMemorySmPersistence::new();
        store
            .upsert_session(fixture_session("stream-1"))
            .await
            .unwrap();
        store
            .append_unacked(fixture_unacked("stream-1", 1))
            .await
            .unwrap();
        store.delete_session("stream-1").await.unwrap();
        assert!(store.get_session("stream-1").await.unwrap().is_none());
        assert!(store.list_unacked("stream-1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_expired_returns_only_past_sessions() {
        let store = InMemorySmPersistence::new();
        let now = Utc::now();
        let mut past = fixture_session("expired");
        past.detached_at = now - chrono::Duration::seconds(120);
        past.max_resume_duration = Duration::from_secs(60);
        let mut future = fixture_session("active");
        future.detached_at = now;
        future.max_resume_duration = Duration::from_secs(600);

        store.upsert_session(past).await.unwrap();
        store.upsert_session(future).await.unwrap();
        let expired = store.list_expired_sessions(now).await.unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].stream_id, "expired");
    }
}
