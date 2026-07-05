use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};

use tracing::debug;

use super::persistence_codec::{
    detached_to_persisted, parse_xml_to_persisted_unacked, persisted_to_detached,
};
use super::{DetachedSession, SmRegistryError, DEFAULT_MAX_SESSIONS};

const STREAM_LOCK_SHARDS: usize = 256;

/// In-memory implementation of the SM session registry, optionally
/// backed by a [`SmPersistenceStorage`] so detached sessions survive
/// process restarts (issue #209 slice (d) phase 3, locked Q8 = B).
///
/// When `persistence` is `Some`, every `store_session` /
/// `take_session` / `cleanup_expired` mutation also writes to the
/// durable backend; on startup, [`Self::restore_from_persistence`]
/// rebuilds the in-memory view so an XEP-0198 `<resume previd='…'/>`
/// finds sessions that detached before the most recent restart.
///
/// Custom Debug skips the persistence handle (the
/// [`SmPersistenceStorage`] trait does not require `Debug`).
pub struct InMemorySmSessionRegistry {
    pub(super) sessions: RwLock<HashMap<String, DetachedSession>>,
    pub(super) claimed_sessions: RwLock<HashMap<String, DetachedSession>>,
    pub(super) stream_locks: Vec<Arc<tokio::sync::Mutex<()>>>,
    pub(super) max_sessions: usize,
    /// Optional durable backing store. When `None` the registry is
    /// strictly in-memory (legacy behaviour); production wiring sets
    /// this via [`Self::with_persistence`] before Arc-wrapping.
    pub(super) persistence:
        Option<std::sync::Arc<dyn super::super::persistence::SmPersistenceStorage>>,
}

impl Default for InMemorySmSessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for InMemorySmSessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemorySmSessionRegistry")
            .field("max_sessions", &self.max_sessions)
            .field(
                "session_count",
                &self.sessions.read().map(|s| s.len()).unwrap_or(0),
            )
            .field(
                "claimed_count",
                &self.claimed_sessions.read().map(|s| s.len()).unwrap_or(0),
            )
            .field("stream_lock_shards", &self.stream_locks.len())
            .field("persistence_attached", &self.persistence.is_some())
            .finish()
    }
}

impl InMemorySmSessionRegistry {
    /// Create a new in-memory registry with default settings.
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            claimed_sessions: RwLock::new(HashMap::new()),
            stream_locks: new_stream_locks(),
            max_sessions: DEFAULT_MAX_SESSIONS,
            persistence: None,
        }
    }

    /// Create a registry with custom settings.
    pub fn with_capacity(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::with_capacity(max_sessions.min(10000))),
            claimed_sessions: RwLock::new(HashMap::new()),
            stream_locks: new_stream_locks(),
            max_sessions,
            persistence: None,
        }
    }

    /// Attach a durable backing store. Must be called once at
    /// construction time before the registry is wrapped in `Arc`.
    /// Subsequent mutating writes are mirrored into `storage`; reads
    /// stay in-memory for hot-path latency.
    pub fn with_persistence(
        mut self,
        storage: std::sync::Arc<dyn super::super::persistence::SmPersistenceStorage>,
    ) -> Self {
        self.persistence = Some(storage);
        self
    }

    /// Rebuild the in-memory view from the attached durable store.
    /// Called on server startup before any traffic is accepted, so
    /// an XEP-0198 `<resume previd='…'/>` for a session that
    /// detached before restart still succeeds.
    ///
    /// Returns the number of sessions hydrated. No-op when no
    /// persistence is attached.
    pub async fn restore_from_persistence(&self) -> Result<usize, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(0);
        };
        let now = chrono::Utc::now();
        // Single round-trip — replaces an N+1 (1 list_all_sessions +
        // N list_unacked) with a single SELECT … LEFT JOIN sm_unacked
        // on backends that override (libSQL/Postgres). In-memory
        // backends fall back to the trait-default N+1 path. Issue
        // #209 PR #405.
        let stored = storage
            .list_all_sessions_with_unacked()
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
        let mut hydrated_sessions = Vec::with_capacity(stored.len());
        let mut expired = 0usize;
        let mut bad_rows = 0usize;
        for (persisted, unacked) in stored {
            // Expired-during-downtime sessions (detached_at +
            // max_resume_duration <= now) are hydrated too (issue
            // #1098): deleting their rows here would silently discard
            // their unacked queues, violating XEP-0198 §5 ("treat
            // unacknowledged stanzas … like stanzas to an unavailable
            // resource"). They are not resumable on the wire —
            // peek/take/claim all gate on `is_expired()` — and the
            // SM-expiry janitor's next `drain_expired` pass runs the
            // promote → confirm chain, which is what finally deletes
            // the durable rows via `confirm_drained`.
            let expires_at = persisted.detached_at
                + chrono::Duration::from_std(persisted.max_resume_duration)
                    .unwrap_or(chrono::Duration::seconds(0));
            if expires_at <= now {
                expired += 1;
            }
            match persisted_to_detached(&persisted, &unacked) {
                Ok(session) => hydrated_sessions.push(session),
                Err(error) => {
                    debug!(
                        stream_id = %persisted.stream_id,
                        error = %error,
                        "skipping persisted session: row decode failed (poison pill)"
                    );
                    bad_rows += 1;
                }
            }
        }
        let hydrated = hydrated_sessions.len();
        {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            for session in hydrated_sessions {
                sessions.insert(session.stream_id.clone(), session);
            }
        }
        debug!(
            hydrated,
            expired, bad_rows, "restored detached SM sessions from persistence"
        );
        Ok(hydrated)
    }
}

impl InMemorySmSessionRegistry {
    /// Helper: delete every durable row for `stream_id` (session +
    /// unacked queue). Returns the underlying error so callers can
    /// adopt a "persist-first" ordering — refuse to mutate the
    /// in-memory map when the durable delete failed, so a transient
    /// storage hiccup doesn't leave an orphaned `sm_sessions` row
    /// that `restore_from_persistence` would resurrect on restart.
    /// (Codex P1 + Copilot + Qodo on PR #344: best-effort silent
    /// swallow allowed durable orphans whenever the in-memory state
    /// had already moved on.)
    pub(super) async fn persist_delete_session(
        &self,
        stream_id: &str,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        storage
            .delete_session(&crate::pending_delivery::SmSessionId::new(
                stream_id.to_string(),
            ))
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    pub(super) async fn persist_detached_session_snapshot(
        &self,
        session: &DetachedSession,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        let persisted = detached_to_persisted(session)?;
        let mut unacked_rows = Vec::with_capacity(session.unacked_stanzas.len());
        for entry in &session.unacked_stanzas {
            unacked_rows.push(parse_xml_to_persisted_unacked(
                &session.stream_id,
                entry.sequence,
                &entry.stanza_xml,
                entry.original_receipt_at,
            )?);
        }
        storage
            .store_session_atomic(persisted, unacked_rows)
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    pub(super) fn stream_lock(
        &self,
        stream_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, SmRegistryError> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stream_id.hash(&mut hasher);
        let shard = (hasher.finish() as usize) % self.stream_locks.len();
        Ok(Arc::clone(&self.stream_locks[shard]))
    }

    pub(super) fn find_session_id_matching(
        &self,
        predicate: impl Fn(&DetachedSession) -> bool,
    ) -> Result<Option<String>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some((stream_id, _)) = sessions.iter().find(|(_, session)| predicate(session)) {
            return Ok(Some(stream_id.clone()));
        }
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Ok(claimed
            .iter()
            .find(|(_, session)| predicate(session))
            .map(|(stream_id, _)| stream_id.clone()))
    }

    pub(super) async fn update_detached_session_snapshot(
        &self,
        stream_id: &str,
        predicate: impl Fn(&DetachedSession) -> bool,
        mutate: impl FnOnce(&mut DetachedSession),
    ) -> Result<bool, SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;

        let current = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions
                .get(stream_id)
                .filter(|session| predicate(session))
                .cloned()
        };
        let current = if current.is_some() {
            current
        } else {
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            claimed
                .get(stream_id)
                .filter(|session| predicate(session))
                .cloned()
        };

        let Some(mut updated) = current else {
            return Ok(false);
        };
        mutate(&mut updated);

        // Durable snapshot first, then publish the same typed state in memory.
        // The stream lock serializes this full-snapshot write with other appends
        // and with claim completion/deletion so an older clone cannot overwrite
        // a newer replay window.
        self.persist_detached_session_snapshot(&updated).await?;

        let updated = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            if sessions.contains_key(stream_id) {
                sessions.insert(stream_id.to_string(), updated);
                return Ok(true);
            }
            updated
        };

        let found_claimed = {
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            if claimed.contains_key(stream_id) {
                claimed.insert(stream_id.to_string(), updated);
                true
            } else {
                false
            }
        };
        if found_claimed {
            return Ok(true);
        }

        // The session vanished from both maps between the stream-lock
        // read and this recheck. The only remover that does NOT take
        // this stream's lock is displacement by `store_session` (jid
        // collision / max_sessions eviction, which holds only the NEW
        // stream's shard lock) — and displaced sessions follow the
        // persist-until-confirmed contract (traits.rs): their durable
        // rows must survive until the promote → confirm_drained chain
        // erases them. The previous fail-closed `persist_delete_session`
        // here (PR #486, guarding against hypothetical lock-free
        // removers resurrecting an already-consumed stream) deleted a
        // displaced session's rows mid-promotion, losing the queue on a
        // crash. Every consuming path (take_session, complete_claim,
        // confirm_drained) takes
        // this stream lock, so the consumed-stream-resurrection concern
        // cannot arise here; deletion stays owned by
        // confirm_drained / the janitor. Worst case is an orphan
        // snapshot row that restore_from_persistence rehydrates and the
        // janitor later promotes — at-least-once, never data loss.
        Ok(false)
    }
}

fn new_stream_locks() -> Vec<Arc<tokio::sync::Mutex<()>>> {
    (0..STREAM_LOCK_SHARDS)
        .map(|_| Arc::new(tokio::sync::Mutex::new(())))
        .collect()
}
