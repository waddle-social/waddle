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

use crate::ownership::{ClaimEpoch, CurrentNodeIdentityGuard, Entity, NodeIdentity};
use crate::auth::AuthenticatedPrincipalRef;
use crate::pending_delivery::SmSessionId;
use crate::Stanza;

/// Immutable ownership context authorizing one clustered SM persistence write.
///
/// The claim epoch is meaningful only together with the exact process
/// incarnation that obtained it. Keeping both values in one typed payload
/// prevents a cached pre-self-fence epoch from being paired with a newer
/// [`NodeIdentity`] after claim-epoch ABA.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SmClaimFence {
    owner: NodeIdentity,
    epoch: ClaimEpoch,
}

impl SmClaimFence {
    pub fn new(owner: NodeIdentity, epoch: ClaimEpoch) -> Self {
        Self { owner, epoch }
    }

    pub fn owner(&self) -> &NodeIdentity {
        &self.owner
    }

    pub fn epoch(&self) -> ClaimEpoch {
        self.epoch
    }
}

/// Errors returned by [`SmPersistenceStorage`] implementations.
#[derive(Debug, Error)]
pub enum SmPersistenceError {
    #[error("SM persistence error: {0}")]
    Other(String),

    /// A row for an exact typed stream id exists but cannot be decoded into
    /// the typed persistence model. Recovery may quarantine that stream's
    /// durable session and unacked queue; ordinary backend errors must never
    /// take that destructive path.
    #[error("corrupt SM persistence for '{stream_id}': {detail}")]
    Corrupt {
        stream_id: SmSessionId,
        detail: String,
    },

    /// A fenced write's own fencing check (ADR-0017 Phase 3 Slice 4,
    /// element 4) observed that this node no longer holds — or never
    /// acquired — the entity's ownership claim at the epoch it believed
    /// was current: the `SELECT ... FOR SHARE` issued inside the write's
    /// own transaction returned zero rows. The write was rolled back
    /// before touching `sm_sessions`/`sm_unacked`. Only ever returned by
    /// the Postgres-fenced implementation (the portable, single-node
    /// implementation has no fencing concept and never returns this).
    ///
    /// Distinct from [`Self::Other`] so Slice 5/6 callers can react to a
    /// lost claim (demote the in-memory session, attempt a fresh
    /// acquire/steal) instead of treating this as an opaque backend
    /// failure.
    ///
    /// `entity` is the typed [`Entity`] the fencing check was performed
    /// against (FIX 2, typed-payloads hard rule) — never a pre-formatted
    /// `String` key. [`Entity`] implements `Display` as the same
    /// `<entity_type_tag>:<id>` encoding the SQL layer uses, so this
    /// variant's `#[error]` text renders identically to the pre-FIX-2
    /// bare-`String` form; only the field's *type* changed.
    #[error(
        "fencing check failed: this node does not hold entity '{entity}' at the expected claim epoch"
    )]
    NotOwner { entity: Entity },

    /// Startup-time misconfiguration (ADR-0017 Phase 3 Slice 4 FIX 4): the
    /// resolved SM-persistence database URL does not match the clustering
    /// subsystem's global database URL while clustering is enabled. The
    /// Postgres-fenced `SmPersistenceStorage`'s fencing `SELECT ... FOR
    /// SHARE` targets `clustering_claims`, which lives in the clustering
    /// global database — clustered SM persistence and the claims tables
    /// must be co-located in the same Postgres database, never two
    /// independently configured ones (a second, unrelated database might
    /// not even have a `clustering_claims` table to fence against).
    ///
    /// Both fields are expected to already be credential-redacted by the
    /// caller before construction (this type has no way to redact a DSN
    /// itself, and must not be trusted to store raw secrets).
    #[error(
        "clustered SM persistence must be co-located with the clustering claims tables: \
         resolved SM database URL ({sm_database_url}) does not match the clustering global \
         database URL ({global_database_url})"
    )]
    ClusterColocationMismatch {
        sm_database_url: String,
        global_database_url: String,
    },

    /// Clustering cannot use the portable SQLite/in-memory persistence
    /// implementation because its ownership fence lives in Postgres.
    #[error(
        "clustered SM persistence requires a postgres:// or postgresql:// database URL; got {sm_database_url}"
    )]
    ClusterRequiresPostgres { sm_database_url: String },

    /// Startup wiring enabled clustering without providing the live claim
    /// store and rotating node identity needed by fenced persistence.
    #[error("clustered SM persistence started without live claim-store handles")]
    ClusterClaimHandlesUnavailable,
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
    pub stream_id: SmSessionId,
    pub user_id: String,
    pub jid: FullJid,
    pub inbound_count: u32,
    pub outbound_count: u32,
    pub last_acked: u32,
    pub replay_gap_through: Option<u32>,
    pub max_resume_time: Option<u32>,
    pub detached_at: chrono::DateTime<chrono::Utc>,
    pub max_resume_duration: Duration,
    pub carbons_enabled: bool,
    pub roster_interested: bool,
    pub blocklist_interested: bool,
    pub presence_available: bool,
    pub presence_show: Option<Show>,
    pub presence_status: Option<String>,
    pub presence_priority: i8,
    /// The resource's presence extension payloads (XEP-0115 `<c/>` caps,
    /// XEP-0319 `<idle/>`, arbitrary extensions) as last broadcast while
    /// available, so a session rehydrated from durable storage (restart or
    /// cross-node resume) relays the resource's own advertisements verbatim
    /// instead of coming back caps-less (issue #1206, follow-up to #1101 /
    /// #1103). Typed arbitrary-XML per the typed-payloads hard rule; the
    /// libSQL/Postgres backend serializes to a single TEXT column on write
    /// and parses back to typed on read.
    pub presence_payloads: Vec<minidom::Element>,
}

/// One row in the `sm_unacked` table — a stanza sent to a session
/// that has not yet been acknowledged.
///
/// The stanza is carried as a typed [`Stanza`] (not wire XML) so this
/// trait sits in front of any I/O serialization boundary; the
/// libSQL/Postgres impl serializes to wire XML internally on write
/// and parses back to typed on read.
#[derive(Debug, Clone)]
pub struct PersistedUnackedStanza {
    /// XEP-0198 stream-id of the owning session.
    pub stream_id: SmSessionId,
    /// Server-side outbound sequence number; ordered ascending.
    pub sequence: u32,
    /// Typed stanza payload — the original outbound stanza so SM
    /// resumption replays bit-identically.
    pub stanza: Box<Stanza>,
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
        stream_id: &SmSessionId,
    ) -> Result<Option<PersistedSession>, SmPersistenceError>;

    /// Delete a session and all its unacked stanzas. Called on
    /// successful `<resumed/>` (the session is now live again, no
    /// longer detached) and on session timeout.
    async fn delete_session(&self, stream_id: &SmSessionId) -> Result<(), SmPersistenceError>;

    /// Delete a session while the caller already holds current-incarnation
    /// authority. Clustered implementations reuse this guard instead of
    /// reacquiring the writer-preferring identity gate; portable stores have
    /// no identity fence and use their ordinary atomic delete.
    async fn delete_session_with_authority(
        &self,
        stream_id: &SmSessionId,
        _authority: &CurrentNodeIdentityGuard,
    ) -> Result<(), SmPersistenceError> {
        self.delete_session(stream_id).await
    }

    /// Delete an undecodable session and its unacked queue as one durable
    /// quarantine operation. Clustered implementations bind the immutable
    /// owner/epoch context into the same transaction; portable implementations
    /// deliberately reuse their already-atomic `delete_session` path.
    async fn quarantine_session(
        &self,
        stream_id: &SmSessionId,
        _expected_fence: &SmClaimFence,
    ) -> Result<(), SmPersistenceError> {
        self.delete_session(stream_id).await
    }

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
        stream_id: &SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, SmPersistenceError>;

    /// Remove the named unacked entries — exact `(stream_id,
    /// sequence)` primary-key matches — and return how many rows were
    /// deleted. Used by the XEP-0424/0425 tombstone scrub (issue
    /// #1145) so a retracted stanza cannot be rehydrated into a
    /// replay queue after a restart. Sequences that do not exist are
    /// ignored (idempotent).
    async fn delete_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, SmPersistenceError>;

    /// Read every unacked stanza for a session in sequence order.
    /// Used by `<resumed/>` to replay and by the Q6 promotion path to
    /// drain on session expiry.
    async fn list_unacked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError>;

    /// Enumerate every persisted session whose `detached_at +
    /// max_resume_duration` is in the past. Used by the janitor that
    /// expires timed-out sessions and by the graceful-shutdown drain.
    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PersistedSession>, SmPersistenceError>;

    /// Enumerate every currently-persisted session, regardless of
    /// expiry. Used by [`InMemorySmSessionRegistry`] on startup to
    /// rebuild the in-memory view from durable storage so an
    /// XEP-0198 `<resume previd='…'/>` finds sessions that detached
    /// before the most recent restart.
    async fn list_all_sessions(&self) -> Result<Vec<PersistedSession>, SmPersistenceError>;

    /// Enumerate every persisted session AND its unacked queue in a
    /// single round-trip. Used by `restore_from_persistence` so cold
    /// startup doesn't issue an N+1 (1 list_all_sessions + N
    /// list_unacked).
    ///
    /// **Best-effort semantics**: a session whose unacked queue
    /// fails to decode (corrupted XML, invalid timestamp, etc.) is
    /// SKIPPED with a debug log; other sessions still appear in the
    /// returned set. This mirrors the prior per-session try-catch
    /// in `restore_from_persistence` so a single poison-pill row
    /// can't brick cold startup. (Greptile/Copilot/Qodo P1 review
    /// on PR #405.)
    ///
    /// Default impl falls back to the N+1 sequence so in-memory
    /// backends keep working without implementing a JOIN; the
    /// libSQL/Postgres backend overrides with a single
    /// `SELECT … LEFT JOIN sm_unacked` query that applies the same
    /// poison-pill skip per-row.
    async fn list_all_sessions_with_unacked(
        &self,
    ) -> Result<Vec<(PersistedSession, Vec<PersistedUnackedStanza>)>, SmPersistenceError> {
        let sessions = self.list_all_sessions().await?;
        let mut out = Vec::with_capacity(sessions.len());
        for session in sessions {
            match self.list_unacked(&session.stream_id).await {
                Ok(unacked) => out.push((session, unacked)),
                Err(error) => {
                    tracing::debug!(
                        stream_id = %session.stream_id,
                        error = %error,
                        "list_all_sessions_with_unacked: skipping session whose unacked \
                         queue failed to decode (poison pill); other sessions continue"
                    );
                }
            }
        }
        Ok(out)
    }

    /// Atomically write a session record + its complete unacked
    /// queue. Either every row commits or none does, so a panic /
    /// process crash mid-batch leaves the durable view consistent
    /// (no half-stored session whose row claims N unacked stanzas but
    /// the table only holds K < N). Implementations must treat the
    /// supplied queue as a full replacement for the stream's prior
    /// unacked rows; appending full snapshots would duplicate replay
    /// stanzas. Default impl falls back to delete + upsert + N
    /// appends without a transaction so backends that don't support
    /// nested ops keep working; the libSQL/Postgres backend overrides
    /// with a `BEGIN; … ; COMMIT;` block via `Database::begin`.
    async fn store_session_atomic(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        let stream_id = session.stream_id.clone();
        self.delete_session(&stream_id).await?;
        self.upsert_session(session).await?;
        for entry in unacked {
            self.append_unacked(entry).await?;
        }
        Ok(())
    }

    /// Atomically persist a detached SM snapshot and its non-secret
    /// authenticated principal reference. Clustered implementations bind
    /// both writes to the exact SM claim fence; portable storage refuses
    /// rather than silently dropping authorization authority.
    async fn store_session_atomic_with_principal(
        &self,
        _principal: &AuthenticatedPrincipalRef,
        _session: PersistedSession,
        _unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        Err(SmPersistenceError::Other(
            "SM principal persistence requires fenced Postgres storage".to_string(),
        ))
    }

    /// Fetch the non-secret principal reference paired with an SM snapshot.
    /// Callers must resolve it against the server auth authority before
    /// producing `<resumed/>`; absence is never a fallback to local state.
    async fn get_session_principal(
        &self,
        _stream_id: &SmSessionId,
    ) -> Result<Option<AuthenticatedPrincipalRef>, SmPersistenceError> {
        Err(SmPersistenceError::Other(
            "SM principal resolution requires fenced Postgres storage".to_string(),
        ))
    }

    /// Atomically increment the persistent promotion-failure counter
    /// for `stream_id` and return the new value. Used by the SM-
    /// expiry janitor (issue #209 finding #14) to break runaway retry
    /// loops when Q6 promotion fails repeatedly for permanent reasons
    /// (disk full, schema corruption, etc.) — once the count crosses a
    /// threshold the caller dead-letters the durable row instead of
    /// preserving it for yet another retry.
    ///
    /// Default impl returns `Ok(0)` — in-memory backends don't track
    /// the counter durably, so the janitor's dead-letter path simply
    /// never trips for them. Production backends override with a
    /// `UPDATE … SET promotion_attempts = promotion_attempts + 1` plus
    /// a follow-up SELECT.
    async fn record_promotion_failure(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<u32, SmPersistenceError> {
        let _ = stream_id;
        Ok(0)
    }

    /// Evict any per-`stream_id` claim-fence cache entry this implementation
    /// keeps as a fencing side channel (ADR-0017 Phase 3 Slice 4's "Epoch
    /// side channel" design note; Slice 5 debt (a)).
    ///
    /// Called by `InMemorySmSessionRegistry` (`session_registry/claims.rs`)
    /// every time this node's `ClaimStore` claim for `stream_id` ends —
    /// `release_claim`/`complete_claim`/`complete_claim_if_resumable`'s
    /// terminal branches, and `invalidate_sessions_for_jid`'s removal of a
    /// claimed session — so a cache keyed by stream_id never outlives the
    /// claim it caches the owner/epoch pair for. Default no-op: the portable
    /// (single-node, in-memory) implementation has no such cache (its
    /// `ClaimStore` is always `InProcessClaimStore`, which is cheap to call
    /// directly on every write); only the Postgres-fenced implementation
    /// (`waddle-server`'s `PostgresFencedSmPersistence`) overrides this to
    /// actually remove the cached cell, so a subsequent fenced write for the
    /// same stream_id after a fresh claim always re-derives its epoch rather
    /// than reusing one issued under a claim this node no longer holds.
    fn evict_claim_cache(&self, stream_id: &SmSessionId, expected_fence: &SmClaimFence) {
        let _ = (stream_id, expected_fence);
    }
}

/// In-memory implementation suitable for tests and as the structural
/// fake that future libSQL/Postgres impls drop in for.
#[derive(Debug, Default)]
pub struct InMemorySmPersistence {
    inner: std::sync::Mutex<InMemoryState>,
}

#[derive(Debug, Default)]
struct InMemoryState {
    sessions: std::collections::HashMap<SmSessionId, PersistedSession>,
    // Per-session unacked queue keyed by stream_id.
    unacked: std::collections::HashMap<SmSessionId, Vec<PersistedUnackedStanza>>,
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
        stream_id: &SmSessionId,
    ) -> Result<Option<PersistedSession>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(guard.sessions.get(stream_id).cloned())
    }

    async fn delete_session(&self, stream_id: &SmSessionId) -> Result<(), SmPersistenceError> {
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
        stream_id: &SmSessionId,
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

    async fn delete_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
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
        queue.retain(|s| !sequences.contains(&s.sequence));
        Ok((before - queue.len()) as u64)
    }

    async fn list_unacked(
        &self,
        stream_id: &SmSessionId,
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

    async fn list_all_sessions(&self) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(guard.sessions.values().cloned().collect())
    }
}

#[cfg(test)]
mod tests;
