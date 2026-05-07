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
use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use thiserror::Error;
use tracing::debug;
use xmpp_parsers::presence::Show;

use crate::Stanza;

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

/// One unacknowledged stanza retained on a detached SM session.
///
/// Carries the XEP-0198 outbound sequence + the serialized stanza
/// XML as the queue did before, plus the **server-side receipt time**
/// of the original stanza (NOT the detach time). The Q6 SM-expiry
/// promotion path consumes `original_receipt_at` when it stamps the
/// XEP-0203 `<delay/>` on a flushed offline replay so the recipient
/// sees the failed delivery's true timestamp per XEP-0203 §4.1 +
/// XEP-0198 §5 line 364.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedUnackedStanza {
    /// XEP-0198 outbound sequence number assigned to this stanza.
    pub sequence: u32,
    /// Serialized stanza XML (re-parsed on demand by the promotion
    /// path; kept as `String` here so the queue doesn't pin the
    /// `xmpp_parsers::Element` representation in memory across
    /// detach windows).
    pub stanza_xml: String,
    /// Server-side receipt time of the original stanza. Used by the
    /// Q6 SM-expiry promotion path for the XEP-0203 `<delay/>` stamp.
    pub original_receipt_at: DateTime<Utc>,
}

/// A detached stream management session.
///
/// Contains all the state needed to resume a stream after disconnection.
#[derive(Debug, Clone)]
pub struct DetachedSession {
    /// The unique stream ID
    pub stream_id: String,
    /// Authenticated user identifier.
    pub user_id: String,
    /// The full JID of the session owner
    pub jid: FullJid,
    /// Server's inbound stanza count at detach time
    pub inbound_count: u32,
    /// Server's outbound stanza count at detach time
    pub outbound_count: u32,
    /// Last acknowledged outbound stanza count
    pub last_acked: u32,
    /// Unacknowledged stanzas (sequence + xml + receipt time).
    /// See [`DetachedUnackedStanza`] for field semantics.
    pub unacked_stanzas: Vec<DetachedUnackedStanza>,
    /// Maximum resumption time in seconds
    pub max_resume_time: Option<u32>,
    /// When the session was detached
    pub detached_at: Instant,
    /// XEP-0280 Message Carbons opt-in at detach time.
    ///
    /// XEP-0198 §5 defines `<resumed/>` as continuing the same stream, so any
    /// per-stream add-ons the client previously enabled (here: carbons) must
    /// survive resumption without requiring the client to re-negotiate them.
    pub carbons_enabled: bool,
    /// RFC 6121 roster-interest state at detach time.
    ///
    /// XEP-0198 resumption continues the same stream, so an already
    /// interested resource remains interested after a successful resume.
    pub roster_interested: bool,
    /// Whether the resource had sent available presence at detach time.
    ///
    /// Presence side effects required by RFC 6121 still apply to detached
    /// XEP-0198 streams that were available when the transport dropped.
    pub presence_available: bool,
    /// Last advertised show value while available.
    pub presence_show: Option<Show>,
    /// Last advertised status text while available.
    pub presence_status: Option<String>,
    /// Last advertised priority while available.
    pub presence_priority: i8,
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
            .filter(|entry| sequence_gt(entry.sequence, client_h))
            .count()
    }

    /// Get the XML payloads that must be resent to a client reporting `h`.
    pub fn stanzas_to_resend(&self, client_h: u32) -> Vec<String> {
        self.unacked_stanzas
            .iter()
            .filter(|entry| sequence_gt(entry.sequence, client_h))
            .map(|entry| entry.stanza_xml.clone())
            .collect()
    }

    /// Record an outbound stanza while this stream is detached.
    /// `original_receipt_at` is the server-side receipt time of the
    /// stanza (NOT the detach time) — consumed by the Q6 SM-expiry
    /// promotion path for the XEP-0203 `<delay/>` stamp.
    pub fn record_detached_outbound(
        &mut self,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) {
        self.outbound_count = self.outbound_count.wrapping_add(1);
        if self.unacked_stanzas.len() >= super::DEFAULT_MAX_UNACKED_QUEUE_SIZE {
            self.unacked_stanzas.remove(0);
        }
        self.unacked_stanzas.push(DetachedUnackedStanza {
            sequence: self.outbound_count,
            stanza_xml,
            original_receipt_at,
        });
    }

    pub fn record_detached_outbound_at(
        &mut self,
        sequence: u32,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) {
        self.outbound_count = self.outbound_count.max(sequence);
        if self
            .unacked_stanzas
            .iter()
            .any(|entry| entry.sequence == sequence)
        {
            return;
        }
        if self.unacked_stanzas.len() >= super::DEFAULT_MAX_UNACKED_QUEUE_SIZE {
            self.unacked_stanzas.remove(0);
        }
        self.unacked_stanzas.push(DetachedUnackedStanza {
            sequence,
            stanza_xml,
            original_receipt_at,
        });
        self.unacked_stanzas.sort_by_key(|entry| entry.sequence);
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
}

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
    sessions: RwLock<HashMap<String, DetachedSession>>,
    claimed_sessions: RwLock<HashMap<String, DetachedSession>>,
    max_sessions: usize,
    /// Optional durable backing store. When `None` the registry is
    /// strictly in-memory (legacy behaviour); production wiring sets
    /// this via [`Self::with_persistence`] before Arc-wrapping.
    persistence: Option<std::sync::Arc<dyn super::persistence::SmPersistenceStorage>>,
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
            max_sessions: DEFAULT_MAX_SESSIONS,
            persistence: None,
        }
    }

    /// Create a registry with custom settings.
    pub fn with_capacity(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::with_capacity(max_sessions.min(10000))),
            claimed_sessions: RwLock::new(HashMap::new()),
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
        storage: std::sync::Arc<dyn super::persistence::SmPersistenceStorage>,
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
        let mut expired_ids = Vec::new();
        let mut bad_rows = 0usize;
        for (persisted, unacked) in stored {
            // Filter expired-during-downtime: detached_at +
            // max_resume_duration <= now means the resume window is
            // already closed. Hydrating these would let them appear
            // resumable on the wire and silently exceed
            // max_sessions, plus they'd be re-loaded on every
            // restart since the in-memory janitor doesn't drain
            // durable rows. Mark for durable deletion below.
            let expires_at = persisted.detached_at
                + chrono::Duration::from_std(persisted.max_resume_duration)
                    .unwrap_or(chrono::Duration::seconds(0));
            if expires_at <= now {
                expired_ids.push(persisted.stream_id.clone());
                continue;
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
        // Best-effort durable cleanup of expired rows. Failures here
        // are non-fatal — the janitor's next pass will retry.
        for stream_id in &expired_ids {
            if let Err(error) = storage.delete_session(stream_id).await {
                debug!(
                    stream_id = %stream_id,
                    error = %error,
                    "failed to delete expired persisted SM session during restore; will retry"
                );
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
            expired = expired_ids.len(),
            bad_rows,
            "restored detached SM sessions from persistence"
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
    async fn persist_delete_session(&self, stream_id: &str) -> Result<(), SmRegistryError> {
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
}

/// Parse a wire-XML fragment back to a typed
/// [`super::persistence::PersistedUnackedStanza`] keyed under
/// `stream_id`. Used by both `store_session` (full batch decode) and
/// `record_outbound_for_detached_stream_at` (per-stanza incremental
/// persist for issue #209 finding #8). Returns an `Internal` error
/// for malformed XML or unknown root elements so the caller can
/// abort the persist without leaving an inconsistent durable view.
fn parse_xml_to_persisted_unacked(
    stream_id: &str,
    sequence: u32,
    stanza_xml: &str,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
) -> Result<super::persistence::PersistedUnackedStanza, SmRegistryError> {
    let element: minidom::Element = stanza_xml.parse().map_err(|e: minidom::Error| {
        SmRegistryError::Internal(format!("parse unacked stanza for persistence: {e}"))
    })?;
    let stanza = match element.name() {
        "message" => crate::Stanza::Message(
            xmpp_parsers::message::Message::try_from(element)
                .map_err(|e| SmRegistryError::Internal(e.to_string()))?,
        ),
        "iq" => crate::Stanza::Iq(
            xmpp_parsers::iq::Iq::try_from(element)
                .map_err(|e| SmRegistryError::Internal(e.to_string()))?,
        ),
        "presence" => crate::Stanza::Presence(
            xmpp_parsers::presence::Presence::try_from(element)
                .map_err(|e| SmRegistryError::Internal(e.to_string()))?,
        ),
        other => {
            return Err(SmRegistryError::Internal(format!(
                "unknown unacked stanza element '{other}'"
            )))
        }
    };
    Ok(super::persistence::PersistedUnackedStanza {
        stream_id: crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
        sequence,
        stanza: Box::new(stanza),
        original_receipt_at,
    })
}

/// Convert a [`DetachedSession`] (in-memory shape) to a
/// [`super::persistence::PersistedSession`] (durable shape) for write
/// to [`SmPersistenceStorage`].
fn detached_to_persisted(
    session: &DetachedSession,
) -> Result<super::persistence::PersistedSession, SmRegistryError> {
    use super::persistence::PersistedSession;
    Ok(PersistedSession {
        stream_id: crate::pending_delivery::SmSessionId::new(session.stream_id.clone()),
        user_id: session.user_id.clone(),
        jid: session.jid.clone(),
        inbound_count: session.inbound_count,
        outbound_count: session.outbound_count,
        last_acked: session.last_acked,
        max_resume_time: session.max_resume_time,
        // `detached_at: Instant` is process-relative; persistence
        // captures the wall-clock moment of the persist write. The
        // skew vs. the actual detach-event time is bounded by the
        // store_session call latency (microseconds in practice).
        detached_at: chrono::Utc::now(),
        max_resume_duration: Duration::from_secs(
            session
                .max_resume_time
                .map(u64::from)
                .unwrap_or(DEFAULT_SESSION_TIMEOUT_SECS),
        ),
        carbons_enabled: session.carbons_enabled,
        roster_interested: session.roster_interested,
        presence_available: session.presence_available,
        presence_show: session.presence_show.clone(),
        presence_status: session.presence_status.clone(),
        presence_priority: session.presence_priority,
    })
}

/// Convert a [`super::persistence::PersistedSession`] + its unacked
/// row set back to a [`DetachedSession`] for the in-memory view.
fn persisted_to_detached(
    persisted: &super::persistence::PersistedSession,
    unacked: &[super::persistence::PersistedUnackedStanza],
) -> Result<DetachedSession, SmRegistryError> {
    // `Instant` cannot be reconstructed from a wall-clock, so we
    // use `Instant::now()` minus the elapsed wall-clock since the
    // persisted detach time. This preserves correct `is_expired`
    // behaviour at the cost of a small bounded skew (the time
    // since the persist write).
    let elapsed_since_detach = chrono::Utc::now()
        .signed_duration_since(persisted.detached_at)
        .to_std()
        .unwrap_or(Duration::ZERO);
    let detached_at = Instant::now()
        .checked_sub(elapsed_since_detach)
        .unwrap_or_else(Instant::now);

    let unacked_stanzas: Vec<DetachedUnackedStanza> = unacked
        .iter()
        .map(|row| {
            let element: minidom::Element = match &*row.stanza {
                crate::Stanza::Message(m) => m.clone().into(),
                crate::Stanza::Iq(iq) => iq.clone().into(),
                crate::Stanza::Presence(p) => p.clone().into(),
            };
            let mut buf = Vec::new();
            element
                .write_to(&mut buf)
                .map_err(|e| SmRegistryError::Internal(format!("serialize unacked stanza: {e}")))?;
            let xml = String::from_utf8(buf)
                .map_err(|e| SmRegistryError::Internal(format!("serialize unacked stanza: {e}")))?;
            Ok(DetachedUnackedStanza {
                sequence: row.sequence,
                stanza_xml: xml,
                original_receipt_at: row.original_receipt_at,
            })
        })
        .collect::<Result<_, SmRegistryError>>()?;

    Ok(DetachedSession {
        stream_id: persisted.stream_id.as_str().to_string(),
        user_id: persisted.user_id.clone(),
        jid: persisted.jid.clone(),
        inbound_count: persisted.inbound_count,
        outbound_count: persisted.outbound_count,
        last_acked: persisted.last_acked,
        unacked_stanzas,
        max_resume_time: persisted.max_resume_time,
        detached_at,
        carbons_enabled: persisted.carbons_enabled,
        roster_interested: persisted.roster_interested,
        presence_available: persisted.presence_available,
        presence_show: persisted.presence_show.clone(),
        presence_status: persisted.presence_status.clone(),
        presence_priority: persisted.presence_priority,
    })
}

#[async_trait]
impl SmSessionRegistry for InMemorySmSessionRegistry {
    async fn store_session(&self, session: DetachedSession) -> Result<(), SmRegistryError> {
        let stream_id = session.stream_id.clone();
        let jid = session.jid.clone();
        // Scope the RwLock guards in a block so they're definitively
        // dropped before any await point. RwLockWriteGuard is not
        // Send, and explicit `drop()` doesn't satisfy the async
        // future's lifetime analysis. Capture eviction victims
        // (jid-collision retain + max_sessions oldest) so we can
        // mirror their durable rows after releasing the lock.
        let mut evicted_stream_ids: Vec<String> = Vec::new();
        let count = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            // Capture jid-collision evictions in `sessions` before
            // retain mutates; same for `claimed`.
            for (id, existing) in sessions.iter() {
                if id != &stream_id && existing.jid == jid {
                    evicted_stream_ids.push(id.clone());
                }
            }
            for (id, existing) in claimed.iter() {
                if id != &stream_id && existing.jid == jid {
                    evicted_stream_ids.push(id.clone());
                }
            }
            sessions.retain(|existing_stream_id, existing| {
                existing_stream_id == &stream_id || existing.jid != jid
            });
            claimed.retain(|existing_stream_id, existing| {
                existing_stream_id != &stream_id && existing.jid != jid
            });

            if sessions.len() >= self.max_sessions {
                // Remove oldest session
                if let Some(oldest_key) = sessions
                    .iter()
                    .min_by_key(|(_, s)| s.detached_at)
                    .map(|(k, _)| k.clone())
                {
                    sessions.remove(&oldest_key);
                    debug!(stream_id = %oldest_key, "Evicted oldest SM session to make room");
                    evicted_stream_ids.push(oldest_key);
                }
            }

            sessions.insert(stream_id.clone(), session.clone());
            sessions.len()
        };
        // Mirror in-memory evictions to durable storage so a restart
        // doesn't resurrect sessions that were displaced by a fresh
        // bind for the same JID or by max_sessions overflow. (Copilot
        // review on PR #344: durable rows for evicted streams must
        // not be silently rehydrated.)
        for evicted in &evicted_stream_ids {
            // Best-effort: failure to delete an evictee row means
            // the next restart MAY resurrect it via
            // restore_from_persistence (until its resume window
            // expires and the restore-time expired-filter drops it).
            // Bubbling the error here would fail the whole detach
            // because an unrelated evictee row couldn't be cleaned;
            // log loudly instead so operators can spot storage
            // health issues.
            if let Err(error) = self.persist_delete_session(evicted).await {
                debug!(
                    stream_id = %evicted,
                    error = %error,
                    "evicted SM session: durable delete failed; row will be \
                     filtered by restore-time expiry check"
                );
            }
        }

        // Persist the detached session + its unacked queue so it
        // survives a server restart per locked Q8 = B. Use the
        // atomic store API (issue #209 PR #405): backends that
        // support transactions wrap the session upsert + N unacked
        // appends in a single BEGIN/COMMIT so a panic / process
        // crash mid-batch leaves the durable view consistent —
        // either every row commits or none does. Backends that
        // don't (in-memory) fall back to the trait default which
        // performs the same upsert + N appends without atomicity.
        if let Some(storage) = &self.persistence {
            let persisted = detached_to_persisted(&session)?;
            // Pre-decode every stanza outside the storage call so
            // any parse failure rolls the whole batch back without
            // a round-trip to the backend.
            let mut unacked_rows = Vec::with_capacity(session.unacked_stanzas.len());
            for entry in &session.unacked_stanzas {
                unacked_rows.push(parse_xml_to_persisted_unacked(
                    &stream_id,
                    entry.sequence,
                    &entry.stanza_xml,
                    entry.original_receipt_at,
                )?);
            }
            storage
                .store_session_atomic(persisted, unacked_rows)
                .await
                .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
        }

        debug!(stream_id = %stream_id, count = count, "Stored detached SM session");
        Ok(())
    }

    async fn take_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        // Persist-first ordering (same rationale as complete_claim):
        // peek to see if the session exists, durably erase, then
        // remove from in-memory. Failure to durably erase aborts
        // the take so the caller can retry without leaving an
        // orphan row in storage that restart would resurrect.
        let exists = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.contains_key(stream_id)
        };
        if !exists {
            debug!(stream_id = %stream_id, "SM session not found");
            return Ok(None);
        }
        self.persist_delete_session(stream_id).await?;
        let removed = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let removed = match sessions.remove(stream_id) {
                Some(session) => {
                    if session.is_expired() {
                        debug!(stream_id = %stream_id, "SM session found but expired");
                        None
                    } else {
                        debug!(stream_id = %stream_id, "Retrieved and removed SM session");
                        Some(session)
                    }
                }
                None => {
                    debug!(stream_id = %stream_id, "SM session not found");
                    None
                }
            };
            self.claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .remove(stream_id);
            removed
        };
        Ok(removed)
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
        // Lock-scoped block so guards drop before the durable
        // delete awaits.
        let drained: Vec<DetachedSession> = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            drain_expired_internal(&mut sessions)
        };
        for session in &drained {
            // Best-effort: cleanup paths log and continue rather
            // than aborting the whole sweep on a single bad row.
            // Restart-time expired-filter still drops anything that
            // slipped through.
            if let Err(error) = self.persist_delete_session(&session.stream_id).await {
                debug!(
                    stream_id = %session.stream_id,
                    error = %error,
                    "expired SM session: durable delete failed in cleanup; \
                     restart-time expiry filter will drop the orphan"
                );
            }
        }
        Ok(drained.len())
    }

    async fn session_count(&self) -> usize {
        self.sessions.read().map(|s| s.len()).unwrap_or(0)
    }

    async fn scrub_unacked_for_tombstone(
        &self,
        target_id: &str,
        archive_jid: &str,
    ) -> Result<usize, SmRegistryError> {
        let mut removed_total = 0usize;
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in sessions.values_mut() {
            removed_total += scrub_session_unacked(session, target_id, archive_jid);
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in claimed.values_mut() {
            removed_total += scrub_session_unacked(session, target_id, archive_jid);
        }
        Ok(removed_total)
    }
}

/// Strip every unacked outbound `<message/>` entry that matches a
/// XEP-0424 / XEP-0425 tombstone. Returns the number of entries
/// removed.
///
/// A cached message is removed iff:
///   1. it is a `<message>` element,
///   2. its `from` or `to` attribute bare-equals `archive_jid` (scope
///      guard — prevents cross-conversation collateral damage when
///      short message ids collide across chats), AND
///   3. either its wire `id` attribute matches `target_id` (1:1 case)
///      or any child `<stanza-id id='…'/>` matches `target_id`
///      (groupchat case where the retraction keyed by the room's
///      XEP-0359 stamp per the "archive id == wire stanza-id"
///      invariant).
///
/// Parse errors and non-message frames are skipped silently — only
/// matching messages are removed.
fn scrub_session_unacked(
    session: &mut DetachedSession,
    target_id: &str,
    archive_jid: &str,
) -> usize {
    let before = session.unacked_stanzas.len();
    session
        .unacked_stanzas
        .retain(|entry| match entry.stanza_xml.parse::<minidom::Element>() {
            Ok(el) => !cached_message_matches_tombstone(&el, target_id, archive_jid),
            Err(_) => true,
        });
    before - session.unacked_stanzas.len()
}

fn cached_message_matches_tombstone(
    el: &minidom::Element,
    target_id: &str,
    archive_jid: &str,
) -> bool {
    if el.name() != "message" {
        return false;
    }
    let in_scope = el
        .attr("from")
        .map(|s| jid_bare_equals(s, archive_jid))
        .unwrap_or(false)
        || el
            .attr("to")
            .map(|s| jid_bare_equals(s, archive_jid))
            .unwrap_or(false);
    if !in_scope {
        return false;
    }
    if el.attr("id") == Some(target_id) {
        return true;
    }
    // XEP-0359 §3 scopes `<stanza-id/>` to `urn:xmpp:sid:0`. Match that
    // namespace explicitly so an unrelated extension element happening
    // to be named "stanza-id" in a different namespace cannot trigger
    // a tombstone scrub (Copilot review on PR #305).
    el.children()
        .any(|c| c.is("stanza-id", "urn:xmpp:sid:0") && c.attr("id") == Some(target_id))
}

fn jid_bare_equals(jid_str: &str, archive_jid: &str) -> bool {
    match jid_str.parse::<jid::Jid>() {
        Ok(jid) => jid.to_bare().to_string() == archive_jid,
        Err(_) => false,
    }
}

impl InMemorySmSessionRegistry {
    fn stanza_to_replay_xml(stanza: &Stanza) -> String {
        let element = stanza.to_element();
        let mut buffer = Vec::new();
        element
            .write_to(&mut buffer)
            .expect("serializing typed stanza should not fail");
        String::from_utf8(buffer).expect("serialized typed stanza is UTF-8")
    }

    /// Remove every expired session and return the detached state in full.
    ///
    /// Callers (notably the server-side janitor) need the JID and stream id
    /// of each expired session so they can run associated cleanup —
    /// removing MUC occupants, evicting routing entries, and discarding
    /// sidecar auth context. `cleanup_expired` only returns a count, which
    /// isn't enough for that work.
    /// Drain every detached + claimed session from the in-memory
    /// view, regardless of expiry status. Intended for the
    /// graceful-shutdown path (issue #209 slice (d) phase 4 +
    /// locked Q8 = B): the server is exiting, so it walks the full
    /// session set and hands each one's unacked queue to the Q6
    /// promotion path before terminating.
    ///
    /// **This method does NOT delete durable rows.** The caller is
    /// expected to invoke [`Self::confirm_drained`] for each
    /// session AFTER its unacked queue has been successfully
    /// promoted. This ordering ensures that if promotion fails
    /// mid-batch (timeout, panic, storage error), the failed
    /// sessions' durable rows survive and a subsequent restart can
    /// retry promotion. (Copilot review on PR #346: previous
    /// implementation deleted durable rows up-front, losing
    /// stanzas on any partial-promotion failure.)
    pub async fn drain_all_for_shutdown(&self) -> Result<Vec<DetachedSession>, SmRegistryError> {
        // Issue #209 finding #3: drain ONLY the detached pool. Sessions
        // in `claimed_sessions` have an in-flight `<resume previd='…'/>`
        // claim — pulling one out here while the resuming connection
        // is between `claim_session` and `complete_claim` causes
        // duplicate delivery: the client receives the SM resume replay
        // AND the shutdown drain re-promotes the same unacked queue
        // through Q6, generating a fresh `pending_delivery` row that
        // re-flushes on the next presence after restart. The in-flight
        // resume is responsible for either completing (which calls
        // `complete_claim` → `persist_delete_session`) or releasing
        // (which puts the session back in the detached pool); either
        // outcome cleans up without needing the shutdown drain to
        // touch claimed sessions. If the resume never completes
        // before the runtime exits, the durable row survives and the
        // restart-time expiry path picks it up on the next janitor
        // pass — the same fail-closed retry semantics already used
        // for `PromotedOutcome::StorageFailure`.
        let drained: Vec<DetachedSession> = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.drain().map(|(_, s)| s).collect()
        };
        Ok(drained)
    }

    /// Snapshot every currently-live SM session id (detached +
    /// claimed). Returns `None` if either internal lock is poisoned
    /// — the caller (claim-expiry janitor) MUST treat that as
    /// "skip this sweep" rather than proceed with a partial set,
    /// since an empty live set would trigger mass-release of every
    /// claim. (Copilot review on PR #360.)
    ///
    /// "Live" here means the session's durable record is still
    /// resumable: its resume window hasn't closed yet OR a resume
    /// claim is in flight. Sessions that have already been drained
    /// and `confirm_drained`'d are absent from this set, and their
    /// `pending_delivery` claims are eligible for orphan recovery.
    pub fn live_session_ids(&self) -> Option<Vec<String>> {
        let sessions = self.sessions.read().ok()?;
        let claimed = self.claimed_sessions.read().ok()?;
        let mut out: Vec<String> = sessions.keys().cloned().collect();
        out.extend(claimed.keys().cloned());
        Some(out)
    }

    /// Confirm that a drained session has been fully promoted —
    /// delete its durable row so a subsequent restart doesn't
    /// resurrect it. Best-effort: failures are logged but not
    /// returned, since at this point the unacked queue has already
    /// been promoted and the recipient will see the message via
    /// pending_delivery flush; a stale durable row would just be
    /// filtered by the restart-time expiry check eventually.
    ///
    /// Pair with [`Self::drain_all_for_shutdown`]: drain returns
    /// the sessions, caller promotes each, caller calls
    /// `confirm_drained` per session after successful promotion.
    pub async fn confirm_drained(&self, stream_id: &str) {
        if let Err(error) = self.persist_delete_session(stream_id).await {
            debug!(
                stream_id = %stream_id,
                error = %error,
                "graceful-shutdown drain: durable delete failed; \
                 restart-time expiry filter will catch the orphan"
            );
        }
    }

    /// Increment the persistent promotion-failure counter for
    /// `stream_id` and return the new value. Used by the SM-expiry
    /// janitor (issue #209 finding #14) to detect runaway retry loops
    /// on permanent storage failures (disk full, schema corruption,
    /// blocklist storage permanently broken). Once the count crosses
    /// the operator-defined threshold the caller dead-letters the
    /// durable row via `confirm_drained` instead of preserving it for
    /// yet another retry pass.
    ///
    /// Returns `Ok(0)` when no persistence backend is configured
    /// (in-memory tests), so the dead-letter path simply never trips.
    pub async fn record_promotion_failure(&self, stream_id: &str) -> Result<u32, SmRegistryError> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(0);
        };
        let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
        persistence
            .record_promotion_failure(&session_id)
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    /// Drain expired sessions from the in-memory view. Returns the
    /// drained sessions for the caller to run Q6 promotion on.
    ///
    /// **Does NOT delete durable rows.** The caller MUST invoke
    /// [`Self::confirm_drained`] for each session AFTER its unacked
    /// queue has been successfully promoted. If promotion fails
    /// mid-batch, the failed sessions' durable rows survive so a
    /// restart can retry. (Copilot review on PR #346: previous
    /// up-front delete lost stanzas on partial-promotion failure.)
    pub async fn drain_expired(&self) -> Result<Vec<DetachedSession>, SmRegistryError> {
        let drained: Vec<DetachedSession> = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            drain_expired_internal(&mut sessions)
        };
        Ok(drained)
    }

    /// Atomically claim a resumable session for a single resume attempt.
    ///
    /// Claimed sessions stay writable by detached fanout so stanzas routed
    /// during the claim-to-registration handoff can be merged into the final
    /// replay batch before the claim is completed.
    pub async fn claim_session(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let Some(session) = sessions.remove(stream_id) else {
            return Ok(None);
        };
        if session.is_expired() {
            return Ok(None);
        }
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if claimed.contains_key(stream_id) {
            sessions.insert(stream_id.to_string(), session);
            return Ok(None);
        }
        claimed.insert(stream_id.to_string(), session.clone());
        Ok(Some(session))
    }

    /// Release a previously claimed session without consuming it.
    pub async fn release_claim(&self, stream_id: &str) -> Result<(), SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let session = claimed.remove(stream_id);
        if let Some(session) = session {
            if !session.is_expired() {
                sessions.insert(stream_id.to_string(), session);
            }
        }
        Ok(())
    }

    /// Complete a previously claimed session, returning the claimed copy with
    /// any stanzas recorded during the handoff and removing detached replay
    /// eligibility from the registry.
    pub async fn complete_claim(
        &self,
        stream_id: &str,
    ) -> Result<Option<SmClaimCompletion>, SmRegistryError> {
        // Persist-first ordering: durably erase the session BEFORE
        // we hand it back to the resuming connection. If the durable
        // delete fails, abort the resume — the in-memory entry stays
        // in claimed_sessions and the caller can retry, or
        // release_claim to put it back. Without persist-first, a
        // successful in-memory completion + failed durable delete
        // would leave an orphan row that
        // `restore_from_persistence` would resurrect on next
        // restart, exposing a stale `<resume previd='…'/>` for an
        // already-live session (Codex P1 + Copilot + Qodo on PR
        // #344).
        let exists = {
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            claimed.contains_key(stream_id)
        };
        if !exists {
            return Ok(None);
        }
        self.persist_delete_session(stream_id).await?;
        // Now remove from in-memory; the durable side has already
        // committed.
        let outcome = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id)
            .map(|session| {
                if session.is_expired() {
                    SmClaimCompletion::Expired(session)
                } else {
                    SmClaimCompletion::Resumed(session)
                }
            });
        Ok(outcome)
    }

    /// Remove a stored detached session only if it has not been claimed by a
    /// resume attempt.
    pub async fn remove_stored_session_if_unclaimed(
        &self,
        stream_id: &str,
    ) -> Result<Option<DetachedSession>, SmRegistryError> {
        // Persist-first ordering: peek + abort if claimed, durably
        // erase, then remove from in-memory. Same rationale as
        // complete_claim — failure to durably erase aborts the
        // operation so a transient storage error doesn't leave an
        // orphan that restart resurrects.
        let exists_unclaimed = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.contains_key(stream_id) && !claimed.contains_key(stream_id)
        };
        if !exists_unclaimed {
            return Ok(None);
        }
        self.persist_delete_session(stream_id).await?;
        let removed = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .remove(stream_id);
        Ok(removed)
    }

    /// Invalidate detached sessions for a FullJID after a fresh bind has
    /// replaced that stream identity.
    pub async fn invalidate_sessions_for_jid(
        &self,
        jid: &FullJid,
    ) -> Result<Vec<DetachedSession>, SmRegistryError> {
        // Persist-first: enumerate matching stream-ids under a brief
        // lock, durably erase each, then remove from in-memory. If
        // any durable erase fails, abort before in-memory mutation
        // so a transient storage error doesn't leave orphans.
        let matching_ids: Vec<String> = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut ids: Vec<String> = sessions
                .iter()
                .filter(|(_, s)| s.jid == *jid)
                .map(|(id, _)| id.clone())
                .collect();
            for (id, s) in claimed.iter() {
                if s.jid == *jid {
                    ids.push(id.clone());
                }
            }
            ids
        };
        for stream_id in &matching_ids {
            self.persist_delete_session(stream_id).await?;
        }
        // Now remove from in-memory (durable side already committed).
        let mut removed = Vec::new();
        {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            for stream_id in &matching_ids {
                if let Some(session) = sessions.remove(stream_id) {
                    removed.push(session);
                }
                if let Some(session) = claimed.remove(stream_id) {
                    removed.push(session);
                }
            }
        }
        Ok(removed)
    }

    /// List detached resources for `bare_jid` that had requested the roster.
    pub async fn interested_detached_resources_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.roster_interested
                    && session.jid.to_bare() == *bare_jid
            })
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| {
                    !session.is_expired()
                        && session.roster_interested
                        && session.jid.to_bare() == *bare_jid
                })
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// Record a stanza for one detached interested resource.
    async fn record_outbound_for_detached_resource(
        &self,
        jid: &FullJid,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in sessions.values_mut() {
            if !session.is_expired() && session.roster_interested && session.jid == *jid {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                return Ok(true);
            }
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in claimed.values_mut() {
            if !session.is_expired() && session.roster_interested && session.jid == *jid {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Record a typed stanza for one detached interested resource.
    pub async fn record_stanza_for_detached_resource(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        self.record_outbound_for_detached_resource(
            jid,
            Self::stanza_to_replay_xml(stanza),
            original_receipt_at,
        )
        .await
    }

    /// Record a typed stanza for one detached resource by exact FullJID,
    /// regardless of roster-interest or presence-availability flags.
    pub async fn record_stanza_for_detached_bound_resource(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        let stanza_xml = Self::stanza_to_replay_xml(stanza);
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in sessions.values_mut() {
            if !session.is_expired() && session.jid == *jid {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                return Ok(true);
            }
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in claimed.values_mut() {
            if !session.is_expired() && session.jid == *jid {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Record a stanza directly against a detached stream id, regardless of
    /// roster-interest or presence-availability flags.
    pub async fn record_outbound_for_detached_stream(
        &self,
        stream_id: &str,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = sessions.get_mut(stream_id) {
            if !session.is_expired() {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                return Ok(true);
            }
            return Ok(false);
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = claimed.get_mut(stream_id) {
            if !session.is_expired() {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                return Ok(true);
            }
            return Ok(false);
        }
        Ok(false)
    }

    pub async fn record_outbound_for_detached_stream_at(
        &self,
        stream_id: &str,
        sequence: u32,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        // Issue #209 finding #8: mirror the in-memory append to the
        // durable persistence layer. Without this, a stanza routed
        // into the second-detach drain (after `store_session` has
        // already snapshotted the unacked queue) lives only in
        // memory; a process crash before resume loses it. Persisting
        // first preserves at-least-once semantics — if the durable
        // append succeeds but the in-memory append doesn't run
        // (shouldn't happen in practice), restart-time
        // `restore_from_persistence` rebuilds the in-memory view
        // from durable state.
        //
        // BUT: pre-check session eligibility before persisting, so a
        // call for an unknown / expired stream_id doesn't durably
        // append a row that has no owning session and can never be
        // cleaned up via `delete_session` (Qodo finding on PR #409 —
        // some persistence backends accept `append_unacked` for any
        // stream_id, leaving orphan rows). Locks are dropped before
        // any `.await` so persistence I/O never holds a lock.
        let eligible = self.detached_session_is_live(stream_id)?;
        if !eligible {
            return Ok(false);
        }

        if let Some(storage) = &self.persistence {
            let persisted = parse_xml_to_persisted_unacked(
                stream_id,
                sequence,
                &stanza_xml,
                original_receipt_at,
            )?;
            storage
                .append_unacked(persisted)
                .await
                .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
        }

        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        if let Some(session) = sessions.get_mut(stream_id) {
            if !session.is_expired() {
                session.record_detached_outbound_at(sequence, stanza_xml, original_receipt_at);
                return Ok(true);
            }
            return Ok(false);
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = claimed.get_mut(stream_id) {
            if !session.is_expired() {
                session.record_detached_outbound_at(sequence, stanza_xml, original_receipt_at);
                return Ok(true);
            }
            return Ok(false);
        }
        Ok(false)
    }

    /// Returns true when `stream_id` names a detached session that is
    /// present (in either the live `sessions` or the `claimed_sessions`
    /// map) and not yet expired. Used as a pre-flight check by
    /// [`Self::record_outbound_for_detached_stream_at`] so we don't
    /// durably persist unacked stanzas for sessions that no longer
    /// exist.
    fn detached_session_is_live(&self, stream_id: &str) -> Result<bool, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = sessions.get(stream_id) {
            return Ok(!session.is_expired());
        }
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Ok(claimed
            .get(stream_id)
            .is_some_and(|session| !session.is_expired()))
    }

    /// List all detached resources for a bare JID, including resources that
    /// were not available at detach time.
    pub async fn detached_resources_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| !session.is_expired() && session.jid.to_bare() == *bare_jid)
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| !session.is_expired() && session.jid.to_bare() == *bare_jid)
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// List detached resources for a bare JID that had XEP-0280 carbons enabled.
    pub async fn detached_carbon_resources_for_user(
        &self,
        bare_jid: &BareJid,
        except: &FullJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| {
                session.carbons_enabled
                    && !session.is_expired()
                    && session.jid.to_bare() == *bare_jid
                    && session.jid != *except
            })
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| {
                    session.carbons_enabled
                        && !session.is_expired()
                        && session.jid.to_bare() == *bare_jid
                        && session.jid != *except
                })
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// List detached resources for `bare_jid` that were available at detach.
    pub async fn available_detached_resources_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<FullJid>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut resources: Vec<FullJid> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.presence_available
                    && session.jid.to_bare() == *bare_jid
            })
            .map(|session| session.jid.clone())
            .collect();
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        resources.extend(
            claimed
                .values()
                .filter(|session| {
                    !session.is_expired()
                        && session.presence_available
                        && session.jid.to_bare() == *bare_jid
                })
                .map(|session| session.jid.clone()),
        );
        Ok(resources)
    }

    /// Record a stanza for one detached resource that was available at detach.
    async fn record_outbound_for_detached_available_resource(
        &self,
        jid: &FullJid,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in sessions.values_mut() {
            if !session.is_expired() && session.presence_available && session.jid == *jid {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                return Ok(true);
            }
        }
        drop(sessions);
        let mut claimed = self
            .claimed_sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        for session in claimed.values_mut() {
            if !session.is_expired() && session.presence_available && session.jid == *jid {
                session.record_detached_outbound(stanza_xml, original_receipt_at);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Record a typed stanza for one detached resource that was available at detach.
    pub async fn record_stanza_for_detached_available_resource(
        &self,
        jid: &FullJid,
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
    ) -> Result<bool, SmRegistryError> {
        self.record_outbound_for_detached_available_resource(
            jid,
            Self::stanza_to_replay_xml(stanza),
            original_receipt_at,
        )
        .await
    }

    /// Return last known rich presence state for a detached available resource.
    pub async fn detached_presence_state(
        &self,
        jid: &FullJid,
    ) -> Result<Option<(Option<Show>, Option<String>, i8)>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some(session) = sessions.values().find(|session| {
            !session.is_expired() && session.presence_available && session.jid == *jid
        }) {
            return Ok(Some((
                session.presence_show.clone(),
                session.presence_status.clone(),
                session.presence_priority,
            )));
        }
        drop(sessions);
        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Ok(claimed
            .values()
            .find(|session| {
                !session.is_expired() && session.presence_available && session.jid == *jid
            })
            .map(|session| {
                (
                    session.presence_show.clone(),
                    session.presence_status.clone(),
                    session.presence_priority,
                )
            }))
    }

    /// Return last known rich presence state for every detached available
    /// resource owned by `bare_jid`.
    pub async fn available_detached_presence_states_for_user(
        &self,
        bare_jid: &BareJid,
    ) -> Result<Vec<(FullJid, Option<Show>, Option<String>, i8)>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;

        let mut states: Vec<(FullJid, Option<Show>, Option<String>, i8)> = sessions
            .values()
            .filter(|session| {
                !session.is_expired()
                    && session.presence_available
                    && session.jid.to_bare() == *bare_jid
            })
            .map(|session| {
                (
                    session.jid.clone(),
                    session.presence_show.clone(),
                    session.presence_status.clone(),
                    session.presence_priority,
                )
            })
            .collect();
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        states.extend(
            claimed
                .values()
                .filter(|session| {
                    !session.is_expired()
                        && session.presence_available
                        && session.jid.to_bare() == *bare_jid
                })
                .map(|session| {
                    (
                        session.jid.clone(),
                        session.presence_show.clone(),
                        session.presence_status.clone(),
                        session.presence_priority,
                    )
                }),
        );
        Ok(states)
    }
}

/// Internal helper: remove expired sessions and return them.
fn drain_expired_internal(sessions: &mut HashMap<String, DetachedSession>) -> Vec<DetachedSession> {
    let expired_keys: Vec<String> = sessions
        .iter()
        .filter_map(|(k, s)| {
            if s.is_expired() {
                Some(k.clone())
            } else {
                None
            }
        })
        .collect();

    let mut drained = Vec::with_capacity(expired_keys.len());
    for key in &expired_keys {
        if let Some(session) = sessions.remove(key) {
            drained.push(session);
        }
    }

    if !drained.is_empty() {
        debug!(
            removed = drained.len(),
            remaining = sessions.len(),
            "Cleaned up expired SM sessions"
        );
    }

    drained
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
        make_test_session_for_jid(stream_id, make_test_jid())
    }

    fn make_test_session_for_jid(stream_id: &str, jid: FullJid) -> DetachedSession {
        DetachedSession {
            stream_id: stream_id.to_string(),
            user_id: "user@example.com".to_string(),
            jid,
            inbound_count: 10,
            outbound_count: 15,
            last_acked: 12,
            unacked_stanzas: vec![
                DetachedUnackedStanza {
                    sequence: 13,
                    stanza_xml: "<msg1/>".to_string(),
                    original_receipt_at: Utc::now(),
                },
                DetachedUnackedStanza {
                    sequence: 14,
                    stanza_xml: "<msg2/>".to_string(),
                    original_receipt_at: Utc::now(),
                },
                DetachedUnackedStanza {
                    sequence: 15,
                    stanza_xml: "<msg3/>".to_string(),
                    original_receipt_at: Utc::now(),
                },
            ],
            max_resume_time: Some(300),
            detached_at: Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
        }
    }

    fn make_test_session_with_unacked(
        stream_id: &str,
        unacked: Vec<(u32, String)>,
    ) -> DetachedSession {
        let now = Utc::now();
        let mut s = make_test_session(stream_id);
        s.unacked_stanzas = unacked
            .into_iter()
            .map(|(sequence, stanza_xml)| DetachedUnackedStanza {
                sequence,
                stanza_xml,
                original_receipt_at: now,
            })
            .collect();
        s
    }

    #[tokio::test]
    async fn xep_0198_scrub_for_tombstone_removes_matching_1on1_message() {
        // XEP-0424 §"prevent further distribution" + XEP-0198 resume
        // safety: when a tombstone is applied, the original
        // `<message id='target'>` must not replay on a recipient's
        // resume. Locks the matcher against false negatives (matching
        // messages must be removed) and false positives (non-matching
        // messages and non-message frames must be preserved). Scoped
        // by the recipient's bare JID so the matcher cannot reach
        // outside the conversation.
        let registry = InMemorySmSessionRegistry::new();
        let session = make_test_session_with_unacked(
            "stream-tomb",
            vec![
                (
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='target' type='chat'><body>secret</body><thread parent='root'>child</thread></message>"
                        .to_string(),
                ),
                (
                    2,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='other' type='chat'><body>safe</body></message>"
                        .to_string(),
                ),
                (3, "<presence/>".to_string()),
                (4, "<iq type='result' id='not-a-message'/>".to_string()),
            ],
        );
        registry.store_session(session).await.unwrap();

        let removed = registry
            .scrub_unacked_for_tombstone("target", "user@example.com")
            .await
            .unwrap();
        assert_eq!(removed, 1, "exactly one matching message should be removed");

        let again = registry
            .peek_session("stream-tomb")
            .await
            .unwrap()
            .expect("session still present");
        assert_eq!(again.unacked_stanzas.len(), 3);
        assert!(
            !again
                .unacked_stanzas
                .iter()
                .any(|entry| entry.stanza_xml.contains("id='target'")),
            "scrubbed message must not appear in queue"
        );
        assert!(
            again
                .unacked_stanzas
                .iter()
                .any(|entry| entry.stanza_xml.contains("id='other'")),
            "non-matching message must remain"
        );
        assert!(
            again
                .unacked_stanzas
                .iter()
                .any(|entry| entry.stanza_xml.contains("<presence")),
            "presence frame must remain (not a message)"
        );
        assert!(
            again
                .unacked_stanzas
                .iter()
                .any(|entry| entry.stanza_xml.contains("<iq")),
            "iq frame must remain (not a message)"
        );
    }

    #[tokio::test]
    async fn xep_0198_detached_replay_preserves_xep_0201_thread_metadata() {
        use xmpp_parsers::message::{Body, Message, MessageType, Thread};

        let registry = InMemorySmSessionRegistry::new();
        let jid = make_test_jid();
        let session = make_test_session_for_jid("stream-threaded-replay", jid.clone());
        registry.store_session(session).await.unwrap();

        let mut msg = Message::new(Some(jid::Jid::from(jid.clone())));
        msg.from = Some(jid::Jid::from(
            "sender@example.com/web".parse::<FullJid>().expect("jid"),
        ));
        msg.id = Some("detached-threaded-message".to_string());
        msg.type_ = MessageType::Chat;
        msg.bodies
            .insert(String::new(), Body("threaded".to_string()));
        msg.thread = Some(Thread("conversation-thread".to_string()));
        msg.payloads.push(
            minidom::Element::builder("thread", "urn:example:other:0")
                .attr("kind", "extension")
                .append("not-xep-0201")
                .build(),
        );

        assert!(registry
            .record_stanza_for_detached_bound_resource(&jid, &Stanza::Message(msg), Utc::now())
            .await
            .unwrap());
        let stored = registry
            .peek_session("stream-threaded-replay")
            .await
            .unwrap()
            .expect("detached session remains");
        let replay = stored
            .unacked_stanzas
            .last()
            .map(|entry| &entry.stanza_xml)
            .expect("recorded replay stanza");
        let element = replay
            .parse::<minidom::Element>()
            .expect("valid stanza xml");

        assert!(element.children().any(|child| {
            child.name() == "thread"
                && child.ns() == "jabber:client"
                && child.text() == "conversation-thread"
        }));
        assert!(element.children().any(|child| {
            child.name() == "thread"
                && child.ns() == "urn:example:other:0"
                && child.text() == "not-xep-0201"
        }));
    }

    #[tokio::test]
    async fn xep_0198_scrub_for_tombstone_matches_groupchat_stanza_id() {
        // Groupchat retractions key off the room's XEP-0359 stanza-id
        // per the "archive id == wire stanza-id" invariant
        // (`archive_groupchat_message`). The cached reflection
        // preserves the sender's original `message.id` AND carries
        // `<stanza-id by='room' id='canonical'/>`; the retraction
        // request targets `canonical`, not the sender's id. The
        // matcher must therefore check stanza-id children too —
        // surfaced by Copilot review on PR #305.
        let registry = InMemorySmSessionRegistry::new();
        let session = make_test_session_with_unacked(
            "stream-muc",
            vec![(
                1,
                "<message xmlns='jabber:client' from='room@conf.example.com/alice' to='user@example.com/resource' id='sender-wire-id' type='groupchat'><body>moderated</body><stanza-id xmlns='urn:xmpp:sid:0' by='room@conf.example.com' id='canonical-archive-id'/></message>"
                    .to_string(),
            )],
        );
        registry.store_session(session).await.unwrap();

        let removed = registry
            .scrub_unacked_for_tombstone("canonical-archive-id", "room@conf.example.com")
            .await
            .unwrap();
        assert_eq!(
            removed, 1,
            "groupchat tombstone keyed by stanza-id must scrub the reflection"
        );
    }

    #[tokio::test]
    async fn xep_0198_scrub_for_tombstone_does_not_cross_conversations() {
        // Two clients independently use `id='msg-1'` in different
        // conversations. Retracting in conversation A must not delete
        // the queued message in conversation B that happens to share
        // the same wire id. Codex P1 review on PR #305.
        let registry = InMemorySmSessionRegistry::new();
        let session = make_test_session_with_unacked(
            "stream-cross",
            vec![
                (
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='msg-1' type='chat'><body>conv-A</body></message>"
                        .to_string(),
                ),
                (
                    2,
                    "<message xmlns='jabber:client' from='carol@elsewhere.com/web' to='user@example.com/resource' id='msg-1' type='chat'><body>conv-B</body></message>"
                        .to_string(),
                ),
            ],
        );
        registry.store_session(session).await.unwrap();

        // Tombstone is scoped to alice@example.com (the sender of
        // conversation A's archive context). The matcher must NOT
        // remove the carol→user message even though it shares the
        // wire id, because alice is neither its `from` nor `to`.
        let removed = registry
            .scrub_unacked_for_tombstone("msg-1", "alice@example.com")
            .await
            .unwrap();
        assert_eq!(
            removed, 1,
            "only the alice-scoped message should be removed"
        );

        let again = registry
            .peek_session("stream-cross")
            .await
            .unwrap()
            .expect("session still present");
        assert!(
            again
                .unacked_stanzas
                .iter()
                .any(|entry| entry.stanza_xml.contains("conv-B")),
            "conversation B's message must survive — different scope"
        );
    }

    #[tokio::test]
    async fn xep_0198_scrub_for_tombstone_ignores_non_xep0359_stanza_id_namespace() {
        // XEP-0359 §3 scopes `<stanza-id/>` to `urn:xmpp:sid:0`. An
        // unrelated extension element that happens to be named
        // "stanza-id" in a different namespace must NOT trigger a
        // tombstone scrub (Copilot review on PR #305).
        let registry = InMemorySmSessionRegistry::new();
        let session = make_test_session_with_unacked(
            "stream-ns",
            vec![(
                1,
                "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com/resource' id='wire-id' type='chat'><body>safe</body><stanza-id xmlns='urn:example:other:0' id='target'/></message>"
                    .to_string(),
            )],
        );
        registry.store_session(session).await.unwrap();

        let removed = registry
            .scrub_unacked_for_tombstone("target", "user@example.com")
            .await
            .unwrap();
        assert_eq!(
            removed, 0,
            "stanza-id in non-XEP-0359 namespace must not be matched"
        );
    }

    #[tokio::test]
    async fn xep_0198_scrub_for_tombstone_handles_no_match() {
        let registry = InMemorySmSessionRegistry::new();
        registry
            .store_session(make_test_session_with_unacked(
                "stream-nomatch",
                vec![(
                    1,
                    "<message xmlns='jabber:client' from='alice@example.com/web' to='user@example.com' id='other' type='chat'><body>x</body></message>"
                        .to_string(),
                )],
            ))
            .await
            .unwrap();
        let removed = registry
            .scrub_unacked_for_tombstone("not-here", "user@example.com")
            .await
            .unwrap();
        assert_eq!(removed, 0);
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
    async fn test_store_session_replaces_existing_session_for_same_full_jid() {
        let registry = InMemorySmSessionRegistry::new();
        let mut first = make_test_session("stream-old");
        first.roster_interested = true;
        let mut second = make_test_session("stream-new");
        second.roster_interested = true;

        registry.store_session(first).await.unwrap();
        registry.store_session(second).await.unwrap();

        assert!(registry.take_session("stream-old").await.unwrap().is_none());
        let current = registry
            .take_session("stream-new")
            .await
            .unwrap()
            .expect("newer detached session should remain");
        assert_eq!(current.stream_id, "stream-new");
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
    async fn test_claimed_session_remains_writable_for_handoff_fanout() {
        let registry = InMemorySmSessionRegistry::new();

        let mut session = make_test_session("stream-claimed");
        session.roster_interested = true;
        let jid = session.jid.clone();
        registry.store_session(session).await.unwrap();

        let claimed = registry
            .claim_session("stream-claimed")
            .await
            .unwrap()
            .expect("claim");
        assert_eq!(claimed.stream_id, "stream-claimed");
        assert_eq!(
            registry.session_count().await,
            0,
            "claimed sessions must move out of the normal detached map"
        );

        assert!(
            registry
                .record_stanza_for_detached_resource(
                    &jid,
                    &{
                        let mut presence = xmpp_parsers::presence::Presence::new(
                            xmpp_parsers::presence::Type::None,
                        );
                        presence
                            .statuses
                            .insert(String::new(), "during-claim".to_string());
                        Stanza::Presence(presence)
                    },
                    Utc::now(),
                )
                .await
                .unwrap(),
            "fanout during resume handoff must write to the claimed session"
        );

        let completed = registry
            .complete_claim("stream-claimed")
            .await
            .unwrap()
            .expect("completed claim");
        match completed {
            SmClaimCompletion::Resumed(completed) => {
                assert!(
                    completed
                        .unacked_stanzas
                        .iter()
                        .any(|entry| entry.stanza_xml.contains("during-claim")),
                    "completed claim must include fanout recorded during handoff"
                );
            }
            SmClaimCompletion::Expired(_) => panic!("claim should still be resumable"),
        }
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
        assert_eq!(registry.session_count().await, 0);
    }

    #[tokio::test]
    async fn test_cleanup_expired() {
        let registry = InMemorySmSessionRegistry::new();

        // Store some sessions
        let mut expired = make_test_session("stream-exp1");
        expired.max_resume_time = Some(0);
        registry.store_session(expired).await.unwrap();

        let valid =
            make_test_session_for_jid("stream-valid", "user@example.com/valid".parse().unwrap());
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
            let session = make_test_session_for_jid(
                &format!("stream-{}", i),
                format!("user@example.com/resource-{i}").parse().unwrap(),
            );
            registry.store_session(session).await.unwrap();
        }

        assert_eq!(registry.session_count().await, 3);

        // Store a 4th - should evict oldest
        let session = make_test_session_for_jid(
            "stream-new",
            "user@example.com/resource-new".parse().unwrap(),
        );
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

    // --- SmPersistenceStorage integration (slice (d) phase 3) -------

    use super::super::persistence::SmPersistenceStorage as _;

    fn realistic_message_stanza(body: &str) -> String {
        // Build a valid XMPP message via the typed builder so the
        // persistence path can parse it back to a typed Stanza on
        // store_session. The fmt-pinned indentation is what the
        // serializer emits when rebuilt via Element::from(message).
        let mut m = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        m.bodies
            .insert(String::new(), xmpp_parsers::message::Body(body.to_string()));
        let element: xmpp_parsers::minidom::Element = m.into();
        let mut buf = Vec::new();
        element.write_to(&mut buf).expect("serialize message");
        String::from_utf8(buf).expect("utf8")
    }

    fn realistic_test_session(stream_id: &str) -> DetachedSession {
        realistic_test_session_for_jid(stream_id, make_test_jid())
    }

    fn realistic_test_session_for_jid(stream_id: &str, jid: FullJid) -> DetachedSession {
        DetachedSession {
            stream_id: stream_id.to_string(),
            user_id: "user@example.com".to_string(),
            jid,
            inbound_count: 4,
            outbound_count: 7,
            last_acked: 5,
            unacked_stanzas: vec![
                DetachedUnackedStanza {
                    sequence: 6,
                    stanza_xml: realistic_message_stanza("first"),
                    original_receipt_at: Utc::now(),
                },
                DetachedUnackedStanza {
                    sequence: 7,
                    stanza_xml: realistic_message_stanza("second"),
                    original_receipt_at: Utc::now(),
                },
            ],
            max_resume_time: Some(120),
            detached_at: Instant::now(),
            carbons_enabled: true,
            roster_interested: true,
            presence_available: true,
            presence_show: Some(Show::Chat),
            presence_status: Some("online".to_string()),
            presence_priority: 3,
        }
    }

    #[tokio::test]
    async fn store_session_mirrors_to_persistence_when_attached() {
        let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
        let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
        let session = realistic_test_session("stream-1");
        registry.store_session(session.clone()).await.unwrap();

        let stream_id = crate::pending_delivery::SmSessionId::new("stream-1");
        let persisted = storage.get_session(&stream_id).await.unwrap().unwrap();
        assert_eq!(persisted.user_id, session.user_id);
        assert_eq!(persisted.jid, session.jid);
        assert_eq!(persisted.inbound_count, session.inbound_count);
        assert_eq!(persisted.outbound_count, session.outbound_count);
        assert_eq!(persisted.last_acked, session.last_acked);
        assert_eq!(persisted.carbons_enabled, session.carbons_enabled);
        let unacked = storage.list_unacked(&stream_id).await.unwrap();
        assert_eq!(unacked.len(), 2);
        let seqs: Vec<u32> = unacked.iter().map(|u| u.sequence).collect();
        assert_eq!(seqs, vec![6, 7]);
    }

    #[tokio::test]
    async fn take_session_deletes_from_persistence() {
        let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
        let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
        registry
            .store_session(realistic_test_session("stream-1"))
            .await
            .unwrap();
        // Resume — should drain durable storage.
        let _ = registry.take_session("stream-1").await.unwrap();
        let stream_id = crate::pending_delivery::SmSessionId::new("stream-1");
        assert!(storage.get_session(&stream_id).await.unwrap().is_none());
        assert!(storage.list_unacked(&stream_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn restore_from_persistence_rebuilds_in_memory_view() {
        let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
        // Pre-populate storage as if a previous server lifecycle had
        // detached two sessions for distinct users. Using distinct
        // JIDs is important: store_session evicts any prior detached
        // session with the same JID (RFC-aligned: a fresh bind for
        // a JID supersedes any older detached stream for that JID),
        // and the durable mirror also deletes the evicted row, so
        // two sessions with the same JID would resolve to one.
        {
            let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
            registry
                .store_session(realistic_test_session_for_jid(
                    "stream-1",
                    "alice@example.com/web".parse().unwrap(),
                ))
                .await
                .unwrap();
            registry
                .store_session(realistic_test_session_for_jid(
                    "stream-2",
                    "bob@example.com/laptop".parse().unwrap(),
                ))
                .await
                .unwrap();
        }
        // Simulate restart: brand-new registry, only persistence
        // attached. The in-memory view starts empty.
        let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
        assert_eq!(registry.session_count().await, 0);

        let hydrated = registry.restore_from_persistence().await.unwrap();
        assert_eq!(hydrated, 2);
        assert_eq!(registry.session_count().await, 2);

        // Both sessions resumable post-restart.
        let resumed = registry.take_session("stream-1").await.unwrap();
        assert!(resumed.is_some());
        let resumed = resumed.unwrap();
        assert_eq!(resumed.unacked_stanzas.len(), 2);
        assert!(resumed.carbons_enabled);
        assert_eq!(resumed.presence_priority, 3);
    }

    #[tokio::test]
    async fn restore_is_noop_when_no_persistence_attached() {
        let registry = InMemorySmSessionRegistry::new();
        assert_eq!(registry.restore_from_persistence().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn complete_claim_deletes_durable_session_on_resume() {
        // The real resume path is claim_session -> complete_claim,
        // not take_session. Without durable cleanup at the
        // complete_claim commitment point, a successful resume
        // would leave rows in storage that restart_from_persistence
        // would resurrect. (Codex P1 + Copilot review on PR #344.)
        let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
        let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
        registry
            .store_session(realistic_test_session("stream-1"))
            .await
            .unwrap();
        let stream_id = crate::pending_delivery::SmSessionId::new("stream-1");
        assert!(storage.get_session(&stream_id).await.unwrap().is_some());

        let _claimed = registry.claim_session("stream-1").await.unwrap();
        let outcome = registry.complete_claim("stream-1").await.unwrap();
        assert!(matches!(outcome, Some(SmClaimCompletion::Resumed(_))));

        assert!(storage.get_session(&stream_id).await.unwrap().is_none());
        assert!(storage.list_unacked(&stream_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn store_session_evicts_jid_collision_durably() {
        // Two store_session calls for the same JID with different
        // stream_ids: the second supersedes the first per RFC
        // resume semantics. The first's durable rows must be
        // deleted too — otherwise restart_from_persistence
        // resurrects the obsolete stream and exposes a stale
        // <resume previd='…'/> path. (Copilot review on PR #344.)
        let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());
        let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
        registry
            .store_session(realistic_test_session_for_jid(
                "stream-old",
                "alice@example.com/web".parse().unwrap(),
            ))
            .await
            .unwrap();
        registry
            .store_session(realistic_test_session_for_jid(
                "stream-new",
                "alice@example.com/web".parse().unwrap(),
            ))
            .await
            .unwrap();
        let old_id = crate::pending_delivery::SmSessionId::new("stream-old");
        let new_id = crate::pending_delivery::SmSessionId::new("stream-new");
        assert!(
            storage.get_session(&old_id).await.unwrap().is_none(),
            "evicted stream-old should be removed from durable storage"
        );
        assert!(
            storage.get_session(&new_id).await.unwrap().is_some(),
            "stream-new should remain"
        );
    }

    #[tokio::test]
    async fn restore_skips_and_deletes_expired_sessions() {
        // Sessions whose resume window already closed during the
        // server's downtime must not be rehydrated, AND their
        // durable rows must be deleted so restart doesn't re-load
        // them next boot. (Copilot review on PR #344.)
        let storage = std::sync::Arc::new(super::super::persistence::InMemorySmPersistence::new());

        // Manually insert an already-expired session by writing
        // directly to storage with a detached_at + duration in the
        // past.
        let now = chrono::Utc::now();
        let expired = super::super::persistence::PersistedSession {
            stream_id: crate::pending_delivery::SmSessionId::new("stream-expired"),
            user_id: "alice".to_string(),
            jid: "alice@example.com/web".parse().unwrap(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            max_resume_time: Some(60),
            detached_at: now - chrono::Duration::seconds(120),
            max_resume_duration: Duration::from_secs(60),
            carbons_enabled: false,
            roster_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
        };
        storage.upsert_session(expired).await.unwrap();

        let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
        let hydrated = registry.restore_from_persistence().await.unwrap();
        assert_eq!(hydrated, 0);
        // Durable cleanup of expired rows.
        assert!(storage
            .get_session(&crate::pending_delivery::SmSessionId::new("stream-expired"))
            .await
            .unwrap()
            .is_none());
    }
}
