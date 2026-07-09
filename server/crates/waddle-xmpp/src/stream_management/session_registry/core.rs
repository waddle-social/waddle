use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::debug;

use crate::ownership::{
    ClaimEpoch, ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity,
    SharedNodeIdentity,
};

use super::persistence_codec::{
    detached_to_persisted, parse_xml_to_persisted_unacked, persisted_to_detached,
};
use super::{DetachedSession, SmRegistryError, DEFAULT_MAX_SESSIONS};

const STREAM_LOCK_SHARDS: usize = 256;

/// Bound on any `ClaimStore` acquire/`ensure_claimed` call made while this
/// registry holds one of its [`STREAM_LOCK_SHARDS`] stream-shard locks (FIX
/// 5, council-adjudicated ADR-0017 Phase 3 Slice 5 corrigenda:
/// `claim_session`, `claims.rs::acquire_claim_store_entry_for_detach`, and
/// [`InMemorySmSessionRegistry::hydrate_reclaimed`] below).
///
/// **Shard-fan-in rationale**: `stream_lock` hashes a stream id down to one
/// of a fixed, small number of shard mutexes — many unrelated stream ids
/// share the same shard. A hung `ClaimStore` call while holding one shard's
/// lock therefore does not just stall the one stream id it was issued for;
/// it stalls every OTHER live stream id that happens to hash to the same
/// shard too (store/take/claim/release, all of which take the same shard
/// lock before touching `sessions`/`claimed_sessions`). This is a strictly
/// wider blast radius than a genuinely per-entity lock would have, which is
/// why every `ClaimStore` call issued under a shard lock is bounded here —
/// mirrors `self_fence.rs::expire_bounded`'s bounded/best-effort/logged
/// pattern one level down (a per-entity claim call instead of a per-node
/// lease call).
pub(super) const CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Exact per-entity outcome of post-reclaim hydration.
///
/// Recovery callers must distinguish a locally usable session from a claim
/// that moved elsewhere, a claim that was terminally released, and a
/// transient failure that must keep the node not-ready. A count alone cannot
/// express those states: `Ok(0)` used to collapse all three and could publish
/// a fresh live claim with no corresponding in-memory session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimedSessionHydration {
    /// The exact current claim is represented in this registry and its epoch
    /// cache is bound to the supplied owner incarnation.
    Hydrated,
    /// The exact current claim was already represented locally; no durable
    /// row was reloaded or overwritten.
    AlreadyLocal,
    /// A read after the reclaim attempt proves a different node owns it.
    Elsewhere,
    /// The exact claim was released (or was already absent) after the durable
    /// session proved absent or unusable.
    TerminallyReleased,
    /// Ownership or storage is uncertain; recovery must retry or rotate the
    /// candidate and must not restore readiness.
    Retry,
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
/// [`SmPersistenceStorage`] trait does not require `Debug`) and the
/// claim store (`dyn ClaimStore` does not require `Debug` either).
pub struct InMemorySmSessionRegistry {
    pub(super) sessions: RwLock<HashMap<String, DetachedSession>>,
    pub(super) claimed_sessions: RwLock<HashMap<String, DetachedSession>>,
    pub(super) stream_locks: Vec<Arc<tokio::sync::Mutex<()>>>,
    pub(super) max_sessions: usize,
    /// Recently applied XEP-0424/0425 tombstones, kept for the
    /// promotion-time re-check (round-2 review R2). Bounded by
    /// [`super::tombstones::RECENT_TOMBSTONE_TTL`] +
    /// [`super::tombstones::MAX_RECENT_TOMBSTONES`].
    pub(super) recent_tombstones: RwLock<Vec<super::tombstones::RecentTombstone>>,
    /// Optional durable backing store. When `None` the registry is
    /// strictly in-memory (legacy behaviour); production wiring sets
    /// this via [`Self::with_persistence`] before Arc-wrapping.
    pub(super) persistence:
        Option<std::sync::Arc<dyn super::super::persistence::SmPersistenceStorage>>,
    /// The entity-ownership authority for this registry's SM-session claims
    /// (ADR-0017 Phase 3 Slice 1, Q2 "retrofit, not wrap"). Defaults to
    /// [`InProcessClaimStore`] — correct for every build today, since no
    /// caller yet constructs this registry with `clustering.enabled`; a
    /// later slice injects a Postgres-backed store via
    /// [`Self::with_claim_store`] once `SmPersistenceStorage` itself
    /// becomes claim-scoped (Slice 4+).
    ///
    /// This is the **authority** on whether a claim is granted
    /// (`claims.rs`'s `claim_session` gates its own outcome on
    /// [`ClaimStore::acquire`]'s result) and on when a claim ends
    /// (`release_claim`, every terminal branch of `complete_claim`/
    /// `complete_claim_if_resumable`, and `invalidate_sessions_for_jid`'s
    /// removal of a claimed session all call back into it). `stream_locks`/
    /// `sessions`/`claimed_sessions` remain exactly the in-process
    /// contention optimization and session-*state* holders the ADR names
    /// for `StreamLockMap` (element 4) — never a second source of
    /// ownership truth alongside this store, which is precisely the
    /// *wrap* design Q2 rejected.
    pub(super) claim_store: Arc<dyn ClaimStore>,
    /// This node's identity, as presented to `claim_store`. Single-node
    /// deployments use a [`SharedNodeIdentity`] wrapping
    /// [`NodeIdentity::local`]; [`Self::with_claim_store`] (ADR-0017 Phase 3
    /// Slice 5) instead wires in the SAME live, updatable handle
    /// `self_fence::run_node_lease` refreshes on every re-registration
    /// (mirroring `PostgresFencedSmPersistence`'s identical Slice 4
    /// follow-up plumbing fix) — every call site reads `.current()` at the
    /// moment it actually needs the identity rather than caching a
    /// snapshot, so a self-fence/re-registration mid-process-lifetime is
    /// observed immediately rather than silently binding claim CAS calls to
    /// a stale, superseded `node_epoch` forever.
    pub(super) node_identity: SharedNodeIdentity,
    /// Tracks the epoch this registry last observed for each currently
    /// claimed SM-session entity, so `release_claim`/`complete_claim` can
    /// hand the right epoch back to `claim_store.release`. Purely local
    /// bookkeeping — the `ClaimStore` implementation itself is the
    /// authority on what epoch is actually current.
    pub(super) claim_epochs: RwLock<HashMap<String, ClaimEpoch>>,
    /// ADR-0017 Phase 3 Slice 6: the cross-node "ask the live owner to
    /// detach" bridge for the XEP-0198 resume path's live-handshake branch.
    /// `None` for single-node/non-clustering deployments (the cross-node
    /// resume fallback then never has anything to ask — see
    /// `cross_node_resume::attempt_cross_node_resume`'s doc comment).
    /// Production wiring injects a `waddle-server`-side adapter over
    /// `RelayHandle` via [`Self::with_remote_resume_asker`].
    pub(super) remote_resume: Option<Arc<dyn super::cross_node_resume::RemoteResumeAsker>>,
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
            .field("node_identity", &self.node_identity.current())
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
            recent_tombstones: RwLock::new(Vec::new()),
            persistence: None,
            claim_store: Arc::new(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            claim_epochs: RwLock::new(HashMap::new()),
            remote_resume: None,
        }
    }

    /// Create a registry with custom settings.
    pub fn with_capacity(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::with_capacity(max_sessions.min(10000))),
            claimed_sessions: RwLock::new(HashMap::new()),
            stream_locks: new_stream_locks(),
            max_sessions,
            recent_tombstones: RwLock::new(Vec::new()),
            persistence: None,
            claim_store: Arc::new(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            claim_epochs: RwLock::new(HashMap::new()),
            remote_resume: None,
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

    /// Inject a `ClaimStore`/live-identity pair other than the single-node
    /// [`InProcessClaimStore`] default (ADR-0017 Phase 3, Q2). Must be
    /// called once at construction time before the registry is wrapped in
    /// `Arc`. ADR-0017 Phase 3 Slice 5 wires this in production
    /// (`server/http.rs::create_sm_session_registry`) with
    /// `ClusteringHandles::claim_pair()`'s pair — the *same* `SharedNodeIdentity`
    /// `self_fence::run_node_lease` updates on every re-registration, not a
    /// one-time snapshot, so this registry's claim calls always bind
    /// whatever identity is currently in force.
    pub fn with_claim_store(
        mut self,
        claim_store: Arc<dyn ClaimStore>,
        me: SharedNodeIdentity,
    ) -> Self {
        self.claim_store = claim_store;
        self.node_identity = me;
        self
    }

    /// Inject the cross-node "ask the live owner to detach" bridge
    /// (ADR-0017 Phase 3 Slice 6). Must be called once at construction time
    /// before the registry is wrapped in `Arc`, exactly like
    /// [`Self::with_claim_store`]. Production wiring
    /// (`server/http.rs::create_sm_session_registry`) sets this alongside
    /// the claim store whenever clustering is enabled; single-node builds
    /// leave it `None`, so `cross_node_resume::attempt_cross_node_resume`'s
    /// live-handshake branch never has anything to ask (byte-identical
    /// single-node behavior).
    pub fn with_remote_resume_asker(
        mut self,
        asker: Arc<dyn super::cross_node_resume::RemoteResumeAsker>,
    ) -> Self {
        self.remote_resume = Some(asker);
        self
    }

    /// Rebuild the in-memory view from the attached durable store.
    /// Called on server startup before any traffic is accepted, so
    /// an XEP-0198 `<resume previd='…'/>` for a session that
    /// detached before restart still succeeds.
    ///
    /// **Startup-time operation only (FIX 2, council-adjudicated ADR-0017
    /// Phase 3 Slice 5 corrigenda)**: this method's unfenced, unscoped
    /// `list_all_sessions_with_unacked` table scan is safe only because
    /// nothing else can plausibly be racing it for a stream id it has not
    /// yet reached — this runs once, before any traffic is accepted. It
    /// MUST NOT be re-run against a live, already-serving registry (the
    /// orphan reaper previously re-ran it after every successful steal,
    /// which re-scans every row this node already holds on every sweep and
    /// — worse — can observe a row a live session concurrently
    /// completes/re-claims mid-scan). [`Self::hydrate_reclaimed`] is the
    /// live-safe alternative for exactly that case: given the specific
    /// entities a caller just proved ownership of (via `steal_stale` or an
    /// equivalent CAS), it hydrates only those, under each one's own
    /// stream-shard lock, with a fresh in-memory absence re-check — never a
    /// table scan, never a blind insert.
    ///
    /// **ADR-0017 Phase 3 Slice 5 — acquire-then-hydrate** (element 9,
    /// quoted verbatim: *"hydrates only sessions whose claim this node
    /// holds or can acquire at startup ... it never performs unscoped
    /// full-table hydration"*): the read below (`list_all_sessions_with_unacked`)
    /// is still a full, unfenced table scan — it has to be, there is no
    /// other way to discover which stream ids exist — but every row is now
    /// gated on a per-entity [`ClaimStore::ensure_claimed`] call before it
    /// is allowed into `self.sessions`. A row this node successfully claims
    /// (a fresh claim on a single-node/first-ever-restore deployment, or a
    /// self-reacquire of this exact node's own pre-restart claim once
    /// `ensure_claimed`'s self-match fires under the *same* `node_id` — see
    /// that method's doc comment) is hydrated; a row genuinely claimed by
    /// a different, still-live node is skipped — that node already has it
    /// in memory (or will, on its own restore pass), and this node MUST NOT
    /// also hydrate a copy (the exact double-ownership hazard this slice
    /// closes). A row whose owner has died is left unclaimed here (a
    /// concurrent restore/steal never matches this node's identity, so it
    /// stays `AlreadyClaimed` against the dead owner until that owner's
    /// `clustering_nodes` row is provably stale) — the **orphan reaper**
    /// (`server::session_janitors::spawn_orphan_reaper_janitor`) is the
    /// mechanism that reclaims those, not this startup pass, since a
    /// dead-owner determination requires the owner-stale predicate this
    /// unfenced per-row read does not evaluate.
    ///
    /// **Restart-time expired-row deletion (element 9/element 4)**: this
    /// slice does *not* add an unscoped delete-on-restore step. Code
    /// research for this slice found no existing unscoped delete to
    /// claim-scope here — issue #1098 deliberately *hydrates* expired
    /// sessions rather than deleting them at restore time, specifically so
    /// their unacked queues still run the Q6 promote → confirm chain
    /// instead of being silently discarded. Deleting a claimed session
    /// eagerly here, before that chain runs, would re-introduce exactly
    /// the data-loss bug #1098 fixed. Once a row is hydrated under this
    /// node's claim, the (now itself claim-scoped, see
    /// `server::session_janitors::spawn_sm_expiry_janitor`) SM-expiry
    /// janitor's `drain_expired`/promote/`confirm_drained` chain is the
    /// sole deletion path, and its writes already run under the row-locked
    /// fenced epoch via `PostgresFencedSmPersistence`. Recorded as
    /// deviation 28 (plan doc; corrected from an earlier "deviation 27"
    /// citation — see the plan's Slice 5 "Design addition (major fix 6)"
    /// paragraph, amended in place to point at 28) — the plan's
    /// major-fix-6 premise of an existing unscoped restore-time delete
    /// does not match this codebase's actual state.
    ///
    /// **Per-row stream-shard-lock discipline (FIX 2)**: each row's
    /// eventual in-memory insert takes that row's own stream-shard lock —
    /// the same lock every other registry mutator (`store_session`,
    /// `take_session`, `claim_session`, …) takes before touching
    /// `sessions`/`claimed_sessions` — and re-checks the stream id is
    /// absent from BOTH maps immediately before inserting. This is cheap
    /// safety for this method's startup-time role (see above): at true
    /// cold start nothing else can have raced ahead, but the same
    /// discipline the live-only [`Self::hydrate_reclaimed`] needs is applied
    /// here too rather than special-cased away, so a row this node's own
    /// Slice-4 lazy first-fenced-write path (or a live detach) already
    /// raced into memory ahead of this scan reaching the same row is
    /// skipped rather than overwritten with a stale durable read.
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
        // #209 PR #405. This read is unfenced/unscoped by necessity (see
        // this method's doc comment) — the per-row `ensure_claimed` call
        // below is what scopes which rows this node is actually allowed to
        // hydrate.
        let stored = storage
            .list_all_sessions_with_unacked()
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
        let mut hydrated = 0usize;
        let mut expired = 0usize;
        let mut bad_rows = 0usize;
        let mut foreign_claims = 0usize;
        let mut already_present = 0usize;
        // Read once per call, not once per row: `restore_from_persistence`
        // only ever runs at startup, well before this node could have
        // self-fenced and re-registered under a fresh identity, but reading
        // through `.current()` here (rather than caching a snapshot for the
        // whole call) keeps this consistent with every other call site's
        // discipline of never holding a stale identity across an `.await`.
        for (persisted, unacked) in stored {
            let identity = self.node_identity.current();
            let entity = Entity::new(
                EntityType::SmSession,
                persisted.stream_id.as_str().to_string(),
            );
            let epoch = match self.claim_store.ensure_claimed(&entity, &identity).await {
                Ok(epoch) => epoch,
                Err(crate::ownership::ClaimError::AlreadyClaimed) => {
                    // Another (live) node already holds this entity's
                    // claim — never hydrate a second in-memory copy. The
                    // orphan reaper, not this pass, is what reclaims a row
                    // whose owner has actually died.
                    foreign_claims += 1;
                    continue;
                }
                Err(error) => {
                    // A transient backend failure: skip this row rather
                    // than failing the whole restore pass. It is retried
                    // on this node's next restart, or reclaimed by the
                    // orphan reaper if its owner (this node, under a
                    // now-superseded identity) is later found stale.
                    debug!(
                        stream_id = %persisted.stream_id,
                        %error,
                        "restore_from_persistence: ClaimStore ensure_claimed failed; \
                         skipping this row for this pass"
                    );
                    continue;
                }
            };
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
            let session = match persisted_to_detached(&persisted, &unacked) {
                Ok(session) => session,
                Err(error) => {
                    debug!(
                        stream_id = %persisted.stream_id,
                        error = %error,
                        "skipping persisted session: row decode failed (poison pill)"
                    );
                    // Claimed above but never hydrated (a genuine
                    // poison-pill row, not a claim conflict) — release the
                    // now-unused claim rather than leak it, so a future
                    // pass (or the orphan reaper, once this identity is
                    // stale) can act on the row again.
                    self.release_claim_store_entry_under(persisted.stream_id.as_str(), epoch)
                        .await;
                    bad_rows += 1;
                    continue;
                }
            };
            // FIX 2: per-row stream-shard-lock discipline (see this
            // method's doc comment) — take this row's own shard lock and
            // re-check both maps immediately before inserting, rather than
            // batching every hydrated row into one insert pass after the
            // loop (the previous shape, which held no lock at all across
            // the whole scan).
            let stream_id = session.stream_id.clone();
            let stream_lock = self.stream_lock(&stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            let present = {
                let sessions = self
                    .sessions
                    .read()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                let claimed = self
                    .claimed_sessions
                    .read()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                sessions.contains_key(&stream_id) || claimed.contains_key(&stream_id)
            };
            if present {
                already_present += 1;
                continue;
            }
            {
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                sessions.insert(stream_id.clone(), session);
            }
            if let Ok(mut claim_epochs) = self.claim_epochs.write() {
                claim_epochs.insert(stream_id, epoch);
            }
            hydrated += 1;
        }
        debug!(
            hydrated,
            expired,
            bad_rows,
            foreign_claims,
            already_present,
            "restored detached SM sessions from persistence"
        );
        Ok(hydrated)
    }

    /// Targeted hydration for freshly-reclaimed SM-session claims (FIX 2,
    /// council-adjudicated ADR-0017 Phase 3 Slice 5 corrigenda) — the
    /// live-safe counterpart to [`Self::restore_from_persistence`] (a
    /// startup-time-only, whole-table operation; see its doc comment).
    /// Callers: the orphan reaper janitor, after a successful
    /// `steal_stale(OwnerStale)` for one or more entities
    /// (`server::session_janitors::run_orphan_reaper_sweep`), and the
    /// inline post-fence reclaim in `self_fence::run_node_lease` (FIX 4),
    /// after this node's own just-superseded identity's claims are stolen
    /// back under the freshly re-registered identity. Neither caller may
    /// re-run `restore_from_persistence` — the server is already serving
    /// live traffic, and an unscoped table scan racing a live session that
    /// completes/re-claims mid-scan is exactly the **live restore hazard**
    /// this method exists to close.
    ///
    /// Per entity, under that entity's own stream-shard lock (never a
    /// table scan, never a blind insert):
    /// 1. Entities whose type is not `SmSession` are skipped (logged) —
    ///    this registry only ever hydrates SM-session claims.
    /// 2. Re-checks the stream id is absent from BOTH `sessions` and
    ///    `claimed_sessions` — if either already holds it (a live session
    ///    completed, another concurrent hydration already landed it, or
    ///    this entity was reclaimed more than once across overlapping
    ///    sweeps), skip: never overwrite a live in-memory copy with a
    ///    stale durable read.
    /// 3. Re-confirms this node still holds the claim via a bounded
    ///    `ClaimStore::ensure_claimed` self-reacquire (FIX 5 — bounded
    ///    because this call runs under the stream-shard lock; see
    ///    [`CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT`]'s doc comment for the
    ///    shard-fan-in rationale) — a defensive re-check rather than
    ///    trusting the caller-supplied epoch blindly, since the caller's
    ///    `steal_stale` may have committed some time before this call
    ///    actually reaches this entity's turn in a batch.
    /// 4. Loads the durable row (`get_session` + `list_unacked`); a
    ///    missing row (already promoted/deleted by a concurrent sweep) is
    ///    a no-op, not an error.
    /// 5. Inserts into `sessions`, recording the epoch `ensure_claimed`
    ///    confirmed in step 3.
    ///
    /// Returns the number of entities actually hydrated — entities skipped
    /// by steps 1-4 are not counted and do not produce an `Err`, mirroring
    /// `restore_from_persistence`'s best-effort, skip-and-continue
    /// semantics for individual rows.
    pub async fn hydrate_reclaimed(
        &self,
        entities: &[(Entity, ClaimEpoch)],
    ) -> Result<usize, SmRegistryError> {
        let identity = self.node_identity.current();
        self.hydrate_reclaimed_as(entities, &identity).await
    }

    /// Targeted hydration under an explicitly proven node incarnation.
    ///
    /// Post-self-fence recovery uses this variant while the process-wide
    /// [`SharedNodeIdentity`] deliberately still points at the fenced epoch:
    /// publishing the new identity before terminal recovery completes would
    /// reopen unrelated claim admission. The caller has already won each
    /// supplied claim for `identity`; this method still performs the same
    /// bounded `ensure_claimed` recheck before hydrating any durable row.
    pub async fn hydrate_reclaimed_as(
        &self,
        entities: &[(Entity, ClaimEpoch)],
        identity: &NodeIdentity,
    ) -> Result<usize, SmRegistryError> {
        let mut hydrated = 0usize;
        for (entity, caller_epoch) in entities {
            if self
                .hydrate_reclaimed_one_as(entity, *caller_epoch, identity)
                .await?
                == ReclaimedSessionHydration::Hydrated
            {
                hydrated += 1;
            }
        }
        Ok(hydrated)
    }

    /// Strict, typed hydration of one exact recovery grant.
    ///
    /// Unlike [`Self::hydrate_reclaimed_as`]'s historical aggregate count,
    /// this method never hides a transient failure inside `Ok(0)`. It is the
    /// recovery state machine's readiness gate: a caller may proceed only on
    /// `Hydrated`, `AlreadyLocal`, `Elsewhere`, or `TerminallyReleased`.
    pub async fn hydrate_reclaimed_one_as(
        &self,
        entity: &Entity,
        caller_epoch: ClaimEpoch,
        identity: &NodeIdentity,
    ) -> Result<ReclaimedSessionHydration, SmRegistryError> {
        if entity.entity_type != EntityType::SmSession {
            debug!(entity = %entity, "hydrate_reclaimed: non-SmSession entity");
            return Ok(ReclaimedSessionHydration::Retry);
        }
        let Some(storage) = &self.persistence else {
            return Ok(ReclaimedSessionHydration::Retry);
        };
        let stream_id = entity.id.clone();
        let stream_lock = self.stream_lock(&stream_id)?;
        let _stream_guard = stream_lock.lock().await;

        // Re-prove the exact owner after the caller's reclaim CAS. If the
        // bounded self-reacquire has an ambiguous outcome, an unlocked
        // current-claim read classifies it without trusting the caller's
        // stale observation.
        let epoch = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.ensure_claimed(entity, identity),
        )
        .await
        {
            Ok(Ok(epoch)) => epoch,
            Ok(Err(error)) => {
                debug!(stream_id = %stream_id, %error, "strict hydration claim recheck failed");
                return Ok(self
                    .classify_reclaimed_owner(entity, identity, caller_epoch)
                    .await);
            }
            Err(_) => {
                tracing::warn!(
                    stream_id = %stream_id,
                    timeout = ?CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                    "strict hydration claim recheck timed out"
                );
                return Ok(self
                    .classify_reclaimed_owner(entity, identity, caller_epoch)
                    .await);
            }
        };

        let present = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.contains_key(&stream_id) || claimed.contains_key(&stream_id)
        };
        if present {
            self.claim_epochs
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .insert(stream_id, epoch);
            return Ok(ReclaimedSessionHydration::AlreadyLocal);
        }

        let session_id = crate::pending_delivery::SmSessionId::new(stream_id.clone());
        let persisted = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            storage.get_session(&session_id),
        )
        .await
        {
            Ok(Ok(Some(row))) => row,
            Ok(Ok(None)) => {
                debug!(stream_id = %stream_id, "strict hydration found no durable session");
                return Ok(self.release_reclaimed_claim(entity, identity, epoch).await);
            }
            Ok(Err(error)) => {
                debug!(stream_id = %stream_id, %error, "strict hydration durable load failed");
                return Ok(ReclaimedSessionHydration::Retry);
            }
            Err(_) => return Ok(ReclaimedSessionHydration::Retry),
        };
        let unacked = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            storage.list_unacked(&session_id),
        )
        .await
        {
            Ok(Ok(rows)) => rows,
            Ok(Err(error)) => {
                debug!(stream_id = %stream_id, %error, "strict hydration unacked load failed");
                return Ok(ReclaimedSessionHydration::Retry);
            }
            Err(_) => return Ok(ReclaimedSessionHydration::Retry),
        };
        let session = match persisted_to_detached(&persisted, &unacked) {
            Ok(session) => session,
            Err(error) => {
                debug!(stream_id = %stream_id, %error, "strict hydration found a poison row");
                return Ok(self.release_reclaimed_claim(entity, identity, epoch).await);
            }
        };

        // Durable reads can span a lease transition. Re-prove the exact
        // owner and grant generation immediately before the synchronous
        // local insert; a session stolen while storage was loading must
        // never be hydrated here under stale authority.
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.current_claim(entity),
        )
        .await
        {
            Ok(Ok(Some(snapshot)))
                if snapshot.owner == *identity
                    && snapshot.claim_epoch == epoch
                    && snapshot.owner_lease_fresh => {}
            Ok(Ok(Some(snapshot))) if snapshot.owner != *identity => {
                return Ok(ReclaimedSessionHydration::Elsewhere);
            }
            Ok(Ok(None)) => return Ok(ReclaimedSessionHydration::TerminallyReleased),
            _ => return Ok(ReclaimedSessionHydration::Retry),
        }

        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        let mut claim_epochs = self
            .claim_epochs
            .write()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        sessions.insert(stream_id.clone(), session);
        claim_epochs.insert(stream_id, epoch);
        Ok(ReclaimedSessionHydration::Hydrated)
    }

    async fn classify_reclaimed_owner(
        &self,
        entity: &Entity,
        identity: &NodeIdentity,
        expected_epoch: ClaimEpoch,
    ) -> ReclaimedSessionHydration {
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.current_claim(entity),
        )
        .await
        {
            Ok(Ok(Some(snapshot)))
                if snapshot.owner == *identity && snapshot.claim_epoch == expected_epoch =>
            {
                // The ownership proof survived but the full hydration path
                // has not run yet. Keep it pending rather than accepting the
                // claim without local state.
                ReclaimedSessionHydration::Retry
            }
            Ok(Ok(Some(snapshot))) if snapshot.owner != *identity => {
                ReclaimedSessionHydration::Elsewhere
            }
            Ok(Ok(None)) => ReclaimedSessionHydration::TerminallyReleased,
            _ => ReclaimedSessionHydration::Retry,
        }
    }

    async fn release_reclaimed_claim(
        &self,
        entity: &Entity,
        identity: &NodeIdentity,
        epoch: ClaimEpoch,
    ) -> ReclaimedSessionHydration {
        let _ = tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.release(entity, identity, epoch),
        )
        .await;
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.current_claim(entity),
        )
        .await
        {
            Ok(Ok(None)) => ReclaimedSessionHydration::TerminallyReleased,
            Ok(Ok(Some(snapshot))) if snapshot.owner != *identity => {
                ReclaimedSessionHydration::Elsewhere
            }
            _ => ReclaimedSessionHydration::Retry,
        }
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

    /// Durably delete the named unacked rows for a stream — exact
    /// `(stream_id, sequence)` matches, idempotent for absent rows.
    ///
    /// Used by the Q6 promotion retry path (round-2 review R4): after
    /// a PARTIAL promotion failure, the successfully promoted stanzas'
    /// `pending_delivery` rows are already committed, so their
    /// `sm_unacked` rows must be erased before the session is
    /// re-inserted for retry — otherwise every janitor tick re-promotes
    /// the whole queue and duplicates the already-queued stanzas.
    /// Ordering is crash-safe: the pending row commits BEFORE its
    /// `sm_unacked` row is deleted here, preserving at-least-once.
    ///
    /// Takes the stream lock so the delete serializes with
    /// detached-append full snapshots that could otherwise resurrect
    /// the rows. No in-memory mutation happens here — the caller owns
    /// the drained session and drops the entries from its local copy.
    pub async fn delete_unacked_sequences(
        &self,
        stream_id: &str,
        sequences: &[u32],
    ) -> Result<u64, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(0);
        };
        if sequences.is_empty() {
            return Ok(0);
        }
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        storage
            .delete_unacked(
                &crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
                sequences,
            )
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
