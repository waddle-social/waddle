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

use crate::ownership::{ClaimEpoch, CurrentNodeIdentityGuard, Entity, EntityType, NodeIdentity};
use crate::pending_delivery::SmSessionId;
use crate::Stanza;

use super::session_registry::SmSessionGenerationId;

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

    #[error("invalid SM unacked-stanza purpose: {detail}")]
    InvalidUnackedPurpose { detail: String },

    /// An atomic snapshot attempt failed before its commit was issued, so the
    /// caller may safely restore the predecessor's bare-row authority.
    #[error("SM snapshot definitely not committed: {0}")]
    SnapshotDefinitelyNotCommitted(String),

    /// A row for an exact typed stream id exists but cannot be decoded into
    /// the typed persistence model. Recovery may quarantine that stream's
    /// durable session and unacked queue; ordinary backend errors must never
    /// take that destructive path.
    #[error("corrupt SM persistence for '{stream_id}': {detail}")]
    Corrupt {
        stream_id: SmSessionId,
        detail: String,
    },

    /// One exact terminal generation is structurally identifiable but its
    /// persisted session or queue cannot be decoded. Recovery may quarantine
    /// only this key and continue with unrelated generations.
    #[error("corrupt terminal SM generation '{key}': {detail}")]
    CorruptTerminal {
        key: SmTerminalGenerationKey,
        detail: String,
    },

    /// A terminal row's generation UUID itself is corrupt. No exact typed key
    /// can be constructed, so recovery must fail closed instead of guessing a
    /// deletion target or silently abandoning durable work.
    #[error("corrupt terminal SM generation identity for stream '{stream_id}': {detail}")]
    CorruptTerminalIdentity {
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

/// Recovery purpose of one durable SM replay row.
///
/// Resume barriers are ordinary, countable XEP-0199 IQ stanzas on the wire,
/// but they are server-internal causal markers rather than application
/// delivery. If a recovered session expires, Q6 discards these rows instead
/// of promoting them into offline delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmUnackedStanzaPurpose {
    Application,
    ResumeBarrier,
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
    /// Typed recovery disposition for this replay row.
    pub purpose: SmUnackedStanzaPurpose,
}

/// A complete, internally consistent durable view of one SM session.
///
/// Construction checks that every unacked row belongs to the session. This
/// keeps the atomic replacement API from accepting a batch whose typed rows
/// would be split across different stream ids by the storage adapter.
#[derive(Debug, Clone)]
pub struct PersistedSmSnapshot {
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
}

impl PersistedSmSnapshot {
    pub fn new(
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<Self, PersistedSmSnapshotError> {
        if let Some(row) = unacked
            .iter()
            .find(|row| row.stream_id != session.stream_id)
        {
            return Err(PersistedSmSnapshotError::UnackedStreamMismatch {
                session_stream_id: session.stream_id,
                unacked_stream_id: row.stream_id.clone(),
            });
        }
        Ok(Self { session, unacked })
    }

    pub fn session(&self) -> &PersistedSession {
        &self.session
    }

    pub fn unacked(&self) -> &[PersistedUnackedStanza] {
        &self.unacked
    }

    pub fn into_parts(self) -> (PersistedSession, Vec<PersistedUnackedStanza>) {
        (self.session, self.unacked)
    }
}

/// Exact durable identity of one non-resumable terminal generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SmTerminalGenerationKey {
    stream_id: SmSessionId,
    generation_id: SmSessionGenerationId,
}

impl SmTerminalGenerationKey {
    pub fn new(stream_id: SmSessionId, generation_id: SmSessionGenerationId) -> Self {
        Self {
            stream_id,
            generation_id,
        }
    }

    pub fn stream_id(&self) -> &SmSessionId {
        &self.stream_id
    }

    pub fn generation_id(&self) -> SmSessionGenerationId {
        self.generation_id
    }
}

impl std::fmt::Display for SmTerminalGenerationKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.stream_id, self.generation_id)
    }
}

/// A displaced SM generation retained only for terminal promotion work.
///
/// Terminal generations are never resumable. Their exact generation key and
/// complete unacked snapshot survive a process restart independently of the
/// one current `sm_sessions` row with the same opaque XEP-0198 stream id.
#[derive(Debug, Clone)]
pub struct PersistedTerminalGeneration {
    key: SmTerminalGenerationKey,
    snapshot: PersistedSmSnapshot,
    promotion_attempts: u32,
}

impl PersistedTerminalGeneration {
    pub fn new(
        key: SmTerminalGenerationKey,
        snapshot: PersistedSmSnapshot,
    ) -> Result<Self, PersistedSmSnapshotError> {
        Self::with_promotion_attempts(key, snapshot, 0)
    }

    pub fn with_promotion_attempts(
        key: SmTerminalGenerationKey,
        snapshot: PersistedSmSnapshot,
        promotion_attempts: u32,
    ) -> Result<Self, PersistedSmSnapshotError> {
        if key.stream_id != snapshot.session.stream_id {
            return Err(PersistedSmSnapshotError::TerminalStreamMismatch {
                key_stream_id: key.stream_id,
                session_stream_id: snapshot.session.stream_id,
            });
        }
        Ok(Self {
            key,
            snapshot,
            promotion_attempts,
        })
    }

    pub fn key(&self) -> &SmTerminalGenerationKey {
        &self.key
    }

    pub fn snapshot(&self) -> &PersistedSmSnapshot {
        &self.snapshot
    }

    pub fn promotion_attempts(&self) -> u32 {
        self.promotion_attempts
    }

    pub fn into_parts(self) -> (SmTerminalGenerationKey, PersistedSmSnapshot, u32) {
        (self.key, self.snapshot, self.promotion_attempts)
    }
}

/// One structurally identifiable result from a terminal-generation scan.
///
/// Exact reads remain strict and return [`SmPersistenceError::CorruptTerminal`]
/// for corrupt contents. Recovery scans instead preserve the exact key of a
/// parseable corrupt generation so the caller can quarantine only that
/// generation under its claim fence while continuing with healthy siblings.
#[derive(Debug, Clone)]
pub enum TerminalGenerationScanEntry {
    Persisted(PersistedTerminalGeneration),
    Corrupt {
        key: SmTerminalGenerationKey,
        detail: String,
    },
}

impl TerminalGenerationScanEntry {
    pub fn key(&self) -> &SmTerminalGenerationKey {
        match self {
            Self::Persisted(terminal) => terminal.key(),
            Self::Corrupt { key, .. } => key,
        }
    }

    pub fn stream_id(&self) -> &SmSessionId {
        self.key().stream_id()
    }
}

/// Structural errors rejected before an atomic persistence transaction begins.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PersistedSmSnapshotError {
    #[error(
        "unacked row for stream '{unacked_stream_id}' cannot belong to session '{session_stream_id}'"
    )]
    UnackedStreamMismatch {
        session_stream_id: SmSessionId,
        unacked_stream_id: SmSessionId,
    },

    #[error(
        "terminal generation for stream '{key_stream_id}' cannot contain session '{session_stream_id}'"
    )]
    TerminalStreamMismatch {
        key_stream_id: SmSessionId,
        session_stream_id: SmSessionId,
    },
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

    /// Delete only while the immutable owner/claim epoch captured before
    /// promotion is still authoritative. Portable stores have no external
    /// ownership table and reuse their atomic delete.
    async fn delete_session_under_fence(
        &self,
        stream_id: &SmSessionId,
        _expected_fence: &SmClaimFence,
    ) -> Result<(), SmPersistenceError> {
        if self.requires_exact_claim_fence() {
            return Err(SmPersistenceError::NotOwner {
                entity: Entity::new(EntityType::SmSession, stream_id.as_str().to_string()),
            });
        }
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

    /// Exact-fence form of [`Self::delete_unacked`].
    async fn delete_unacked_under_fence(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
        _expected_fence: &SmClaimFence,
    ) -> Result<u64, SmPersistenceError> {
        if self.requires_exact_claim_fence() {
            return Err(SmPersistenceError::NotOwner {
                entity: Entity::new(EntityType::SmSession, stream_id.as_str().to_string()),
            });
        }
        self.delete_unacked(stream_id, sequences).await
    }

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

    /// Enumerate the typed identities of every current durable session without
    /// decoding its payload. Startup uses this key-only inventory to discover
    /// streams before acquiring their claims; corrupt session contents must
    /// not make an otherwise readable identity invisible.
    async fn list_session_ids(&self) -> Result<Vec<SmSessionId>, SmPersistenceError> {
        Ok(self
            .list_all_sessions()
            .await?
            .into_iter()
            .map(|session| session.stream_id)
            .collect())
    }

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

    /// Atomically replace the one resumable row and, when supplied, archive
    /// its same-id predecessor as exact generation-keyed terminal work.
    ///
    /// A successful commit makes both effects visible together: the
    /// successor is the only resumable session, while the predecessor remains
    /// independently promotable and can never be confused with the successor
    /// even when both queues contain the same sequence numbers. A failure
    /// before commit must leave both prior durable views unchanged.
    ///
    /// The default is deliberately fail-closed for the displaced case. Trait
    /// wrappers that only customize ordinary snapshots continue to delegate
    /// through [`Self::store_session_atomic`], but a production backend must
    /// override this method before it can durably displace a same-id
    /// predecessor.
    async fn replace_resumable_session_atomic(
        &self,
        successor: PersistedSmSnapshot,
        displaced_same_id: Option<PersistedTerminalGeneration>,
    ) -> Result<(), SmPersistenceError> {
        if displaced_same_id.is_some() {
            return Err(SmPersistenceError::SnapshotDefinitelyNotCommitted(
                "persistence backend does not implement atomic terminal-generation replacement"
                    .to_string(),
            ));
        }
        let (session, unacked) = successor.into_parts();
        self.store_session_atomic(session, unacked).await
    }

    /// Atomically write a session record + its complete unacked queue.
    ///
    /// Implementations must treat the supplied queue as a full replacement
    /// for the stream's prior unacked rows. The old non-transactional trait
    /// fallback was unsafe despite the method name; implementations that do
    /// not provide a real atomic operation now fail before mutating storage.
    async fn store_session_atomic(
        &self,
        _session: PersistedSession,
        _unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        Err(SmPersistenceError::SnapshotDefinitelyNotCommitted(
            "persistence backend does not implement atomic SM snapshots".to_string(),
        ))
    }

    /// Read one exact non-resumable terminal generation and its queue.
    async fn get_terminal_generation(
        &self,
        _key: &SmTerminalGenerationKey,
    ) -> Result<Option<PersistedTerminalGeneration>, SmPersistenceError> {
        Err(SmPersistenceError::Other(
            "persistence backend does not implement terminal generations".to_string(),
        ))
    }

    /// Enumerate terminal generations for restart recovery.
    async fn list_terminal_generations(
        &self,
    ) -> Result<Vec<TerminalGenerationScanEntry>, SmPersistenceError> {
        Err(SmPersistenceError::Other(
            "persistence backend does not implement terminal generations".to_string(),
        ))
    }

    /// Enumerate terminal generations for one stream id.
    ///
    /// Reclaimed hydration uses this targeted form; the global scan above is
    /// reserved for startup recovery.
    async fn list_terminal_generations_for_stream(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<TerminalGenerationScanEntry>, SmPersistenceError> {
        Ok(self
            .list_terminal_generations()
            .await?
            .into_iter()
            .filter(|entry| entry.stream_id() == stream_id)
            .collect())
    }

    /// Delete one exact terminal generation and its queue atomically.
    async fn delete_terminal_generation(
        &self,
        _key: &SmTerminalGenerationKey,
    ) -> Result<(), SmPersistenceError> {
        Err(SmPersistenceError::Other(
            "persistence backend does not implement terminal generations".to_string(),
        ))
    }

    /// Exact-fence form of [`Self::delete_terminal_generation`].
    async fn delete_terminal_generation_under_fence(
        &self,
        key: &SmTerminalGenerationKey,
        _expected_fence: &SmClaimFence,
    ) -> Result<(), SmPersistenceError> {
        if self.requires_exact_claim_fence() {
            return Err(SmPersistenceError::NotOwner {
                entity: Entity::new(EntityType::SmSession, key.stream_id().as_str().to_string()),
            });
        }
        self.delete_terminal_generation(key).await
    }

    /// Quarantine a corrupt, structurally identifiable terminal generation.
    /// The exact key and claim fence prevent poison recovery from deleting a
    /// same-id successor or sibling generation.
    async fn quarantine_terminal_generation(
        &self,
        key: &SmTerminalGenerationKey,
        expected_fence: &SmClaimFence,
    ) -> Result<(), SmPersistenceError> {
        self.delete_terminal_generation_under_fence(key, expected_fence)
            .await
    }

    /// Delete exact sequence rows from one terminal generation only.
    async fn delete_terminal_unacked(
        &self,
        _key: &SmTerminalGenerationKey,
        _sequences: &[u32],
    ) -> Result<u64, SmPersistenceError> {
        Err(SmPersistenceError::Other(
            "persistence backend does not implement terminal generations".to_string(),
        ))
    }

    /// Exact-fence form of [`Self::delete_terminal_unacked`].
    async fn delete_terminal_unacked_under_fence(
        &self,
        key: &SmTerminalGenerationKey,
        sequences: &[u32],
        _expected_fence: &SmClaimFence,
    ) -> Result<u64, SmPersistenceError> {
        if self.requires_exact_claim_fence() {
            return Err(SmPersistenceError::NotOwner {
                entity: Entity::new(EntityType::SmSession, key.stream_id().as_str().to_string()),
            });
        }
        self.delete_terminal_unacked(key, sequences).await
    }

    /// Increment the retry counter for one exact terminal generation.
    async fn record_terminal_promotion_failure(
        &self,
        _key: &SmTerminalGenerationKey,
    ) -> Result<u32, SmPersistenceError> {
        Err(SmPersistenceError::Other(
            "persistence backend does not implement terminal generations".to_string(),
        ))
    }

    /// Exact-fence form of [`Self::record_terminal_promotion_failure`].
    async fn record_terminal_promotion_failure_under_fence(
        &self,
        key: &SmTerminalGenerationKey,
        _expected_fence: &SmClaimFence,
    ) -> Result<u32, SmPersistenceError> {
        if self.requires_exact_claim_fence() {
            return Err(SmPersistenceError::NotOwner {
                entity: Entity::new(EntityType::SmSession, key.stream_id().as_str().to_string()),
            });
        }
        self.record_terminal_promotion_failure(key).await
    }

    /// Whether a bare stream id still owns either resumable or terminal work.
    /// Claim release must not occur while this returns true.
    async fn has_durable_work(&self, stream_id: &SmSessionId) -> Result<bool, SmPersistenceError> {
        if self.get_session(stream_id).await?.is_some() {
            return Ok(true);
        }
        Ok(self
            .list_terminal_generations()
            .await?
            .iter()
            .any(|entry| entry.stream_id() == stream_id))
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

    /// Exact-fence form of [`Self::record_promotion_failure`].
    async fn record_promotion_failure_under_fence(
        &self,
        stream_id: &SmSessionId,
        _expected_fence: &SmClaimFence,
    ) -> Result<u32, SmPersistenceError> {
        if self.requires_exact_claim_fence() {
            return Err(SmPersistenceError::NotOwner {
                entity: Entity::new(EntityType::SmSession, stream_id.as_str().to_string()),
            });
        }
        self.record_promotion_failure(stream_id).await
    }

    /// Whether generation-sensitive mutations require an exact claim fence.
    fn requires_exact_claim_fence(&self) -> bool {
        false
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
    promotion_attempts: std::collections::HashMap<SmSessionId, u32>,
    terminal_generations:
        std::collections::HashMap<SmTerminalGenerationKey, PersistedTerminalGeneration>,
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
        guard.promotion_attempts.remove(stream_id);
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

    async fn list_session_ids(&self) -> Result<Vec<SmSessionId>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let mut stream_ids = guard.sessions.keys().cloned().collect::<Vec<_>>();
        stream_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        Ok(stream_ids)
    }

    async fn replace_resumable_session_atomic(
        &self,
        successor: PersistedSmSnapshot,
        displaced_same_id: Option<PersistedTerminalGeneration>,
    ) -> Result<(), SmPersistenceError> {
        let successor_stream_id = successor.session().stream_id.clone();
        if let Some(displaced) = displaced_same_id.as_ref() {
            if displaced.key().stream_id() != &successor_stream_id {
                return Err(SmPersistenceError::SnapshotDefinitelyNotCommitted(
                    "terminal predecessor and resumable successor have different stream ids"
                        .to_string(),
                ));
            }
        }

        let (successor_session, successor_unacked) = successor.into_parts();
        let mut guard = self.inner.lock().map_err(|error| {
            SmPersistenceError::SnapshotDefinitelyNotCommitted(error.to_string())
        })?;

        if let Some(mut displaced) = displaced_same_id {
            displaced.promotion_attempts = guard
                .promotion_attempts
                .remove(&successor_stream_id)
                .unwrap_or(displaced.promotion_attempts);
            guard
                .terminal_generations
                .insert(displaced.key.clone(), displaced);
            // A successor is a new logical generation even though it reuses
            // the same opaque stream id, so predecessor retry history must
            // not leak into its resumable row.
            guard.promotion_attempts.remove(&successor_stream_id);
        }

        guard
            .sessions
            .insert(successor_stream_id.clone(), successor_session);
        guard.unacked.insert(successor_stream_id, successor_unacked);
        Ok(())
    }

    async fn store_session_atomic(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        let snapshot = PersistedSmSnapshot::new(session, unacked).map_err(|error| {
            SmPersistenceError::SnapshotDefinitelyNotCommitted(error.to_string())
        })?;
        self.replace_resumable_session_atomic(snapshot, None).await
    }

    async fn get_terminal_generation(
        &self,
        key: &SmTerminalGenerationKey,
    ) -> Result<Option<PersistedTerminalGeneration>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        Ok(guard.terminal_generations.get(key).cloned())
    }

    async fn list_terminal_generations(
        &self,
    ) -> Result<Vec<TerminalGenerationScanEntry>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let mut terminals = guard
            .terminal_generations
            .values()
            .cloned()
            .collect::<Vec<_>>();
        terminals.sort_by(|left, right| {
            left.key
                .stream_id
                .as_str()
                .cmp(right.key.stream_id.as_str())
                .then_with(|| {
                    left.key
                        .generation_id
                        .as_uuid()
                        .as_bytes()
                        .cmp(right.key.generation_id.as_uuid().as_bytes())
                })
        });
        Ok(terminals
            .into_iter()
            .map(TerminalGenerationScanEntry::Persisted)
            .collect())
    }

    async fn list_terminal_generations_for_stream(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<TerminalGenerationScanEntry>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let mut terminals = guard
            .terminal_generations
            .iter()
            .filter(|(key, _)| key.stream_id() == stream_id)
            .map(|(_, terminal)| terminal.clone())
            .collect::<Vec<_>>();
        terminals.sort_by(|left, right| {
            left.key
                .generation_id
                .as_uuid()
                .as_bytes()
                .cmp(right.key.generation_id.as_uuid().as_bytes())
        });
        Ok(terminals
            .into_iter()
            .map(TerminalGenerationScanEntry::Persisted)
            .collect())
    }

    async fn delete_terminal_generation(
        &self,
        key: &SmTerminalGenerationKey,
    ) -> Result<(), SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        guard.terminal_generations.remove(key);
        Ok(())
    }

    async fn delete_terminal_unacked(
        &self,
        key: &SmTerminalGenerationKey,
        sequences: &[u32],
    ) -> Result<u64, SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let Some(terminal) = guard.terminal_generations.get_mut(key) else {
            return Ok(0);
        };
        let before = terminal.snapshot.unacked.len();
        terminal
            .snapshot
            .unacked
            .retain(|row| !sequences.contains(&row.sequence));
        Ok((before - terminal.snapshot.unacked.len()) as u64)
    }

    async fn record_promotion_failure(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<u32, SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        if !guard.sessions.contains_key(stream_id) {
            return Ok(0);
        }
        let attempts = guard
            .promotion_attempts
            .entry(stream_id.clone())
            .or_default();
        *attempts = attempts.saturating_add(1);
        Ok(*attempts)
    }

    async fn record_terminal_promotion_failure(
        &self,
        key: &SmTerminalGenerationKey,
    ) -> Result<u32, SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let Some(terminal) = guard.terminal_generations.get_mut(key) else {
            return Ok(0);
        };
        terminal.promotion_attempts = terminal.promotion_attempts.saturating_add(1);
        Ok(terminal.promotion_attempts)
    }

    async fn has_durable_work(&self, stream_id: &SmSessionId) -> Result<bool, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        Ok(guard.sessions.contains_key(stream_id)
            || guard
                .terminal_generations
                .keys()
                .any(|key| key.stream_id() == stream_id))
    }
}

#[cfg(test)]
mod tests;
