//! Postgres-fenced XEP-0198 stream-management persistence (ADR-0017 Phase 3
//! Slice 4, element 1).
//!
//! **Locked spec** (element 1, quoted verbatim — the phase plan calls this
//! "the phase's most explicit do-not-improvise text"): *"Cluster mode
//! selects a Postgres-only fenced implementation of `SmPersistenceStorage`
//! — a full second implementation of the trait built on `Database::begin`,
//! not a decorator: the portable impl acquires a pooled connection per
//! statement (`ConnectionGuard`), so no wrapper can place the `FOR SHARE`
//! fencing lock in the same transaction as the inner impl's writes, and
//! multi-statement methods (`delete_session`, `store_session_atomic`,
//! `record_promotion_failure`) each need the fencing SELECT inside one
//! `Transaction`. Two trait-shape divergences from the portable impl are
//! explicit and accepted: (a) detach writes ignore the caller-supplied
//! `detached_at` and stamp/re-read Postgres `now()`... (b) expiry listing
//! evaluates the window in SQL against Postgres `now()`, treating the
//! trait's `now` parameter as advisory in the fenced impl. The portable
//! impl and schema remain byte-identical for SQLite."*
//!
//! This is a **sibling** module to [`crate::sm_persistence`], not a
//! submodule of it — "not a decorator" extends to file layout too. It
//! implements `waddle_xmpp::stream_management::persistence::
//! SmPersistenceStorage` for [`PostgresFencedSmPersistence`], a completely
//! independent type from `crate::sm_persistence::DatabaseSmPersistence`.
//! No schema changes: this impl reads/writes the exact same
//! `sm_sessions`/`sm_unacked` tables as the portable impl (byte-identical
//! for SQLite, which never runs this type at all — this module is
//! Postgres-only and gated behind the `clustering` Cargo feature).
//!
//! # Fencing design (per-method)
//!
//! Every method that writes `sm_sessions`/`sm_unacked` on behalf of an
//! SM-session entity runs its own `SELECT 1 FROM clustering_claims WHERE
//! entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR
//! SHARE` — the exact
//! fencing-transaction SQL shape ADR-0017 Phase 3 Slice 1 locks — as the
//! first statement inside the *same* [`crate::db::Transaction`] as the
//! write(s) it guards, all on the **main pool**
//! ([`crate::db::Database::begin`]), never the control-plane pool (the
//! Slice 0/4 pool-assignment rule: a lock and the write it protects must
//! share one connection). If the SELECT observes zero rows, the write
//! aborts (the transaction is dropped, rolling back) **before** any write
//! statement runs, and the method returns
//! [`SmPersistenceError::NotOwner`].
//!
//! # Epoch side channel (design decision, not explicit in the ADR text)
//!
//! The fencing SELECT needs a claim epoch to bind as `$mine`. Per the
//! phase plan's Slice 4 design note, a `ClaimStore` trait method taking a
//! borrowed `waddle-server`-local `Transaction` cannot be expressed on the
//! `waddle-xmpp`-hosted `ClaimStore` trait without an illegal reverse
//! dependency — so this impl issues the fencing SQL inline, itself, and
//! uses [`waddle_xmpp::ownership::ClaimStore`] purely as a side channel
//! for the immutable owner/epoch fence: [`PostgresFencedSmPersistence`] holds an
//! `Arc<dyn ClaimStore>` and a per-stream-id cache
//! (`claim_fences: DashMap<SmSessionId, Arc<tokio::sync::OnceCell<SmClaimFence>>>`),
//! populated lazily — the first fenced write for a stream-id this process
//! instance has not yet seen ensures a claim once and caches the resulting
//! exact process identity together with its epoch; every subsequent fenced
//! write for that stream-id reuses the inseparable pair. A failed fencing
//! check or identity rotation invalidates the cache entry, so a later retry
//! re-acquires instead of pairing a stale epoch with a new incarnation.
//!
//! ## FIX 1 — `ensure_claimed` + per-key single-flight (council-adjudicated)
//!
//! The epoch-population side channel does not call `ClaimStore::acquire`
//! directly: it calls [`waddle_xmpp::ownership::ClaimStore::ensure_claimed`]
//! (acquire, or — on conflict — an idempotent self-reacquire iff the
//! existing row's owner is exactly this node's current identity), and the
//! per-stream-id cache cell is a `tokio::sync::OnceCell`, not a bare
//! `DashMap` entry, so concurrent callers for the *same* stream-id
//! single-flight onto one in-flight `ensure_claimed` call rather than each
//! independently racing `acquire`. Together these close two hazards a bare
//! `acquire`-per-write-attempt design would hit:
//!
//! - **Concurrent first writes for the same not-yet-claimed stream_id**:
//!   two tasks both calling [`Self::claim_fence_for`] for a fresh
//!   `stream_id` fetch (or insert) the same `Arc<OnceCell<SmClaimFence>>`
//!   from `claim_fences` (`DashMap::entry`, briefly locking only that
//!   shard — no lock held across the subsequent `.await`), then both call
//!   `OnceCell::get_or_try_init` on it: exactly one of them actually runs
//!   the `ensure_claimed` future; the other awaits that same attempt's
//!   result. Even in the (rarer) case where two *separate* cells raced
//!   into existence, `ensure_claimed`'s self-reacquire path still saves
//!   correctness — both calls carry the same [`SharedNodeIdentity`]
//!   snapshot, so the loser of the underlying CAS observes its own
//!   node/epoch already on the row and returns the same epoch instead of
//!   erroring.
//! - **The Slice 5/6 self-lock**: once a later slice's `<enable/>`-time
//!   code acquires a claim under this node's identity *before* this path's
//!   first fenced write ever runs for that stream-id, a bare `acquire`
//!   here would spuriously fail with `AlreadyClaimed` against this
//!   process's *own* just-created row. `ensure_claimed` observes the
//!   self-match and returns the row's current epoch instead (deviation
//!   26).
//!
//! `OnceCell::get_or_try_init` leaves the cell uninitialized on error, so a
//! later, separate call to [`Self::claim_fence_for`] retries fresh (a
//! failed attempt is never permanently cached) — this is orthogonal to
//! `assert_fenced`'s own invalidation, which removes an *already-populated*
//! cell from `claim_fences` entirely once a live fencing check reveals the
//! cached fence is stale, forcing the next call to build a brand-new cell.
//!
//! **Slice 5 debt (a), closed**: cells whose init failed (foreign owner)
//! were already never inserted (the `OnceCell` stays uninitialized), and
//! `assert_fenced`/`delete_session` already evicted a stale/deleted
//! session's cell. What was missing — a session ending by any *other*
//! terminal path (a normal resume completion, an explicit release, or a
//! claim lost to `invalidate_sessions_for_jid`) never evicted its cell —
//! is now closed via [`SmPersistenceStorage::evict_claim_cache`]
//! (overridden below): `InMemorySmSessionRegistry`
//! (`session_registry/claims.rs`) calls it on every terminal claim-ending
//! path, so a cell never survives past the `ClaimStore` claim it caches the
//! epoch for.
//!
//! **Interim bootstrapping gap, now closed (Slice 5)**: prior to this
//! slice, nothing in production called `SmPersistenceStorage` methods
//! against a genuinely claim-scoped session — `restore_from_persistence`
//! hydrated unscoped, and no orphan reaper existed. Both now acquire a real
//! `ClaimStore` claim before hydrating (`session_registry/core.rs`'s
//! acquire-then-hydrate, `server/session_janitors.rs`'s orphan reaper), so
//! a session that outlives a process restart is correctly re-claimed under
//! the restarted process's fresh node identity via `ensure_claimed`'s
//! self-reacquire path once that same node's own restore/reaper pass claims
//! it — see those modules' doc comments for the full lifecycle.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::sync::OnceCell;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::auth::{AuthContextId, AuthContextVersion, PrincipalAuthEpoch};
use waddle_xmpp::ownership::{
    ClaimError, ClaimStore, CurrentNodeIdentityGuard, Entity, EntityType, SharedNodeIdentity,
};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    PersistedSession, PersistedUnackedStanza, SmClaimFence, SmPersistenceError,
    SmPersistenceStorage,
};

use crate::db::{Database, DatabaseDriver, Transaction};
use crate::sm_persistence::codec::{
    decode_session, decode_unacked, serialize_presence_payloads, serialize_stanza, show_wire_str,
};

const STALE_CLAIM_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

fn remove_sm_claim_cell_if(
    claim_fences: &DashMap<SmSessionId, Arc<OnceCell<SmClaimFence>>>,
    stream_id: &SmSessionId,
    expected_cell: &Arc<OnceCell<SmClaimFence>>,
) {
    claim_fences.remove_if(stream_id, |_, current| Arc::ptr_eq(current, expected_cell));
}

fn remove_sm_claim_fence_if(
    claim_fences: &DashMap<SmSessionId, Arc<OnceCell<SmClaimFence>>>,
    stream_id: &SmSessionId,
    expected: &SmClaimFence,
) {
    claim_fences.remove_if(stream_id, |_, cell| cell.get() == Some(expected));
}

/// FIX 3: map a [`ClaimError`] to the [`SmPersistenceError`] this trait's
/// callers expect. Only a genuine ownership loss —
/// [`ClaimError::AlreadyClaimed`] (another node holds the entity) or
/// [`ClaimError::Conflict`] (a steal/fence CAS lost the race) — becomes
/// [`SmPersistenceError::NotOwner`]; that variant's own doc comment scopes
/// it to fencing-loss only, so a transient backend outage
/// ([`ClaimError::Backend`]) or a poisoned in-process lock
/// ([`ClaimError::Poisoned`]) must never masquerade as ownership loss —
/// both map to [`SmPersistenceError::Other`] instead. Matched exhaustively
/// (no wildcard arm), so adding a future `ClaimError` variant forces this
/// mapping to be revisited rather than silently defaulting into either
/// bucket.
fn claim_error_to_sm_persistence_error(error: ClaimError, entity: Entity) -> SmPersistenceError {
    match error {
        // ADR-0017 Phase 3 Slice 10: `Draining` (this node refused a NEW
        // claim while marked draining) gets the same treatment as
        // `AlreadyClaimed`/`Conflict` — this node is not, and for
        // `Draining` will not become, the owner, so the caller should treat
        // it exactly like any other ownership loss.
        ClaimError::AlreadyClaimed
        | ClaimError::Conflict
        | ClaimError::Draining
        | ClaimError::AuthorityDisabled => SmPersistenceError::NotOwner { entity },
        ClaimError::Backend(_) | ClaimError::Poisoned => {
            SmPersistenceError::Other(error.to_string())
        }
        // Defensive only: `ensure_claimed`/`acquire` never actually return
        // this variant — it is exclusive to the steal-intent path, which
        // never applies to `EntityType::SmSession` claims (Slice 3 rule 1)
        // — but the match stays exhaustive rather than adding a wildcard.
        ClaimError::SmSessionExcludedFromStealIntent => {
            SmPersistenceError::Other(error.to_string())
        }
    }
}

/// Injective `(entity_type, id) -> TEXT` encoding for the
/// `clustering_claims.entity` column, mirroring
/// `clustering::claims::entity_key` exactly. Duplicated here rather than
/// imported: per the Slice 4 design note, this impl owns its own inline
/// fencing SQL instead of delegating to `ClaimStore`, so it also owns its
/// own copy of the key encoding it binds into that SQL (the accepted
/// duplication tradeoff of the two types living in the crate that
/// actually owns the transaction). `EntityType::as_db_str` is `pub`, so
/// only the trivial `"{tag}:{id}"` format shape is duplicated, not the tag
/// strings themselves.
fn sm_session_entity_key(stream_id: &SmSessionId) -> String {
    format!(
        "{}:{}",
        EntityType::SmSession.as_db_str(),
        stream_id.as_str()
    )
}

fn sm_session_entity(stream_id: &SmSessionId) -> Entity {
    Entity::new(EntityType::SmSession, stream_id.as_str().to_string())
}

/// Postgres-only, `clustering`-fenced [`SmPersistenceStorage`].
///
/// Schema (identical to `crate::sm_persistence::DatabaseSmPersistence` —
/// see that module's doc comment for the full column list): `sm_sessions`,
/// `sm_unacked`. This type's [`Self::open`] creates them with
/// `CREATE TABLE IF NOT EXISTS` so it works whether or not the portable
/// impl already touched the same database (e.g. in tests), but never
/// alters an existing schema — no migration dance, since this impl is
/// Postgres-only and net-new (no pre-Slice-4 fenced deployment can exist
/// to migrate from).
pub struct PostgresFencedSmPersistence {
    db: Database,
    claim_store: Arc<dyn ClaimStore>,
    node_identity: SharedNodeIdentity,
    /// Per-stream-id single-flight cached immutable claim fence — see the module
    /// doc's "Epoch side channel" / FIX 1 sections. Each cell resolves at
    /// most once to a real epoch; a failed resolution leaves the cell
    /// empty (retried fresh on the next call), and `assert_fenced` removes
    /// an already-populated, now-stale cell from the map entirely so a
    /// later call builds a brand-new one.
    claim_fences: Arc<DashMap<SmSessionId, Arc<OnceCell<SmClaimFence>>>>,
}

impl PostgresFencedSmPersistence {
    fn remove_claim_cell_if(
        &self,
        stream_id: &SmSessionId,
        expected_cell: &Arc<OnceCell<SmClaimFence>>,
    ) {
        remove_sm_claim_cell_if(&self.claim_fences, stream_id, expected_cell);
    }

    fn remove_claim_fence_if(&self, stream_id: &SmSessionId, expected: &SmClaimFence) {
        remove_sm_claim_fence_if(&self.claim_fences, stream_id, expected);
    }

    /// Open against an already-opened Postgres [`Database`] handle.
    ///
    /// FIX 4: this constructor no longer opens its own, independent pool
    /// from `WADDLE_XMPP_SM_DATABASE_URL` — the fencing `SELECT ... FOR
    /// SHARE` this impl issues (`assert_fenced`) targets
    /// `clustering_claims`, which lives in the clustering **global**
    /// database, so a second, independently-resolved SM-persistence
    /// database would fence against a table that may not even exist
    /// there. Callers MUST pass the same `Database` handle
    /// `clustering::start_if_enabled` was given (see
    /// [`crate::sm_persistence::open_for_cluster_mode`], the sole
    /// production call site, which also enforces the co-location
    /// invariant before ever reaching this constructor).
    ///
    /// Rejects any non-Postgres handle: this implementation has no
    /// meaningful SQLite/in-memory mode (`FOR SHARE` fencing does not
    /// exist there), unlike the portable impl's `open`.
    pub async fn open(
        db: Database,
        claim_store: Arc<dyn ClaimStore>,
        node_identity: SharedNodeIdentity,
    ) -> Result<Self, SmPersistenceError> {
        if db.driver() != DatabaseDriver::Postgres {
            return Err(SmPersistenceError::Other(format!(
                "PostgresFencedSmPersistence requires a Postgres-backed Database handle; \
                 got driver {:?}",
                db.driver()
            )));
        }
        let storage = Self {
            db,
            claim_store,
            node_identity,
            claim_fences: Arc::new(DashMap::new()),
        };
        storage.ensure_schema().await?;
        tracing::info!(
            "Postgres-fenced SM persistence storage initialized (ADR-0017 Phase 3 Slice 4)"
        );
        Ok(storage)
    }

    async fn ensure_schema(&self) -> Result<(), SmPersistenceError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS sm_sessions (
                stream_id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                full_jid TEXT NOT NULL,
                inbound_count BIGINT NOT NULL,
                outbound_count BIGINT NOT NULL,
                last_acked BIGINT NOT NULL,
                max_resume_secs BIGINT,
                detached_at_ms BIGINT NOT NULL,
                max_resume_duration_ms BIGINT NOT NULL,
                carbons_enabled INTEGER NOT NULL,
                roster_interested INTEGER NOT NULL,
                blocklist_interested INTEGER NOT NULL DEFAULT 0,
                presence_available INTEGER NOT NULL,
                presence_show TEXT,
                presence_status TEXT,
                presence_priority INTEGER NOT NULL,
                replay_gap_through BIGINT,
                promotion_attempts INTEGER NOT NULL DEFAULT 0,
                presence_payloads TEXT
            )
            "#,
            (),
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        // #1206: presence extension payloads (XEP-0115 caps, XEP-0319 idle,
        // ...). ADD COLUMN IF NOT EXISTS (not folded into CREATE TABLE alone)
        // so a table left by an earlier run of this impl gains the column —
        // matching the sibling column-group migration below.
        conn.execute(
            "ALTER TABLE sm_sessions ADD COLUMN IF NOT EXISTS presence_payloads TEXT",
            (),
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS sm_unacked (
                stream_id TEXT NOT NULL,
                sequence BIGINT NOT NULL,
                stanza_xml TEXT NOT NULL,
                original_receipt_at_ms BIGINT NOT NULL,
                PRIMARY KEY (stream_id, sequence)
            )
            "#,
            (),
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        // The principal reference deliberately contains only a bare JID and
        // opaque context/version/epoch metadata. It is never a Session,
        // credential, bearer proof, or token. Every writer/deleter below
        // touches this table in the same transaction as `sm_sessions`.
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS sm_session_principals (
                stream_id TEXT PRIMARY KEY,
                bare_jid TEXT NOT NULL,
                auth_context_id UUID NOT NULL,
                auth_context_version BIGINT NOT NULL,
                principal_auth_epoch BIGINT NOT NULL
            )
            "#,
            (),
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_sm_sessions_detached ON sm_sessions (detached_at_ms)",
            (),
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        // ADR-0017 Phase 3 Slice 5 / element 5 — schema-only groundwork,
        // byte-identical to the portable impl's identical migration (see
        // `sm_persistence/schema.rs`'s doc comment for the full rationale;
        // nothing populates these columns yet). `ADD COLUMN IF NOT EXISTS`
        // rather than folding into the `CREATE TABLE` text above: this
        // impl is net-new (Slice 4), but the table may already exist from
        // an earlier run of this exact impl before this column group
        // landed, and `CREATE TABLE IF NOT EXISTS` is a no-op against an
        // existing table.
        for column_def in [
            "origin_stream_id TEXT",
            "inbound_seq BIGINT",
            "pair_sequence BIGINT",
        ] {
            conn.execute(
                &format!("ALTER TABLE sm_unacked ADD COLUMN IF NOT EXISTS {column_def}"),
                (),
            )
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        }
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_sm_unacked_dedup \
             ON sm_unacked (stream_id, origin_stream_id, inbound_seq) \
             WHERE origin_stream_id IS NOT NULL AND inbound_seq IS NOT NULL",
            (),
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(())
    }

    /// The immutable owner/epoch fence this process holds for `stream_id`,
    /// ensuring a claim on first use. The identity captured by the successful
    /// ensure is never replaced with a later `SharedNodeIdentity` value.
    async fn claim_fence_for(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<SmClaimFence, SmPersistenceError> {
        // FIX 1: fetch-or-insert this stream_id's single-flight cell.
        // `DashMap::entry` briefly locks only the shard for this key; the
        // `Arc` clone below is used after that guard has already dropped,
        // so no lock is held across the `.await` that follows.
        let cell = self
            .claim_fences
            .entry(stream_id.clone())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();

        let identity = self.node_identity.current();
        let entity = sm_session_entity(stream_id);
        // `ensure_claimed`, not a bare `acquire` (FIX 1): idempotent for a
        // self-reacquire under this exact node/epoch — see the module
        // doc's FIX 1 section for why that matters here. `OnceCell::
        // get_or_try_init` supplies the single-flight itself: only one
        // concurrent caller for this cell actually runs the future below;
        // every other concurrent caller awaits that same attempt's result.
        // A failed attempt leaves the cell uninitialized, so a later,
        // separate call retries fresh rather than being poisoned forever.
        let result = cell
            .get_or_try_init(|| async {
                let epoch = self.claim_store.ensure_claimed(&entity, &identity).await?;
                Ok::<_, ClaimError>(SmClaimFence::new(identity.clone(), epoch))
            })
            .await;

        match result {
            Ok(fence) if self.node_identity.current() == *fence.owner() => Ok(fence.clone()),
            Ok(fence) => {
                // `ensure_claimed` may have committed under the previous
                // incarnation immediately before self-fence rotation. Keep
                // the resolved cell as exact retry inventory until that old
                // owner+epoch is confirmed gone; otherwise a current-
                // identity retry conflicts with our own stranded claim.
                match tokio::time::timeout(
                    STALE_CLAIM_RELEASE_TIMEOUT,
                    self.claim_store
                        .release_exact(&entity, fence.owner(), fence.epoch()),
                )
                .await
                {
                    Ok(Ok(
                        waddle_xmpp::ownership::ExactReleaseOutcome::Released
                        | waddle_xmpp::ownership::ExactReleaseOutcome::NotOwned,
                    )) => {
                        self.remove_claim_cell_if(stream_id, &cell);
                        Err(SmPersistenceError::NotOwner { entity })
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(
                            stream_id = %stream_id,
                            %error,
                            "PostgresFencedSmPersistence: stale-incarnation exact claim cleanup \
                             failed; retaining the immutable fence for retry"
                        );
                        Err(claim_error_to_sm_persistence_error(error, entity))
                    }
                    Err(_) => {
                        tracing::warn!(
                            stream_id = %stream_id,
                            timeout = ?STALE_CLAIM_RELEASE_TIMEOUT,
                            "PostgresFencedSmPersistence: stale-incarnation exact claim cleanup \
                             timed out; retaining the immutable fence for retry"
                        );
                        Err(SmPersistenceError::Other(
                            "stale-incarnation exact claim cleanup timed out".to_string(),
                        ))
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    stream_id = %stream_id,
                    %error,
                    "PostgresFencedSmPersistence: claim ensure_claimed failed; this node \
                     cannot fence writes for this SM session"
                );
                Err(claim_error_to_sm_persistence_error(error, entity))
            }
        }
    }

    /// Take the ADR-0017 element-4 fencing lock inside `tx` — the
    /// caller's own [`Database::begin`] transaction — for `stream_id` at the
    /// exact immutable owner/epoch pair. Returns `Ok(())` if this node still holds the claim (the
    /// `FOR SHARE` SELECT observed a row); returns
    /// [`SmPersistenceError::NotOwner`] and invalidates the cached epoch
    /// otherwise. Callers must not perform any write before this returns
    /// `Ok(())`; on `Err`, callers return immediately and let `tx` drop
    /// (rolling back) rather than committing.
    async fn assert_fenced(
        &self,
        tx: &mut Transaction<'_>,
        stream_id: &SmSessionId,
        fence: &SmClaimFence,
    ) -> Result<CurrentNodeIdentityGuard, SmPersistenceError> {
        let authority = self
            .node_identity
            .guard_if_current(fence.owner())
            .await
            .ok_or_else(|| {
                self.remove_claim_fence_if(stream_id, fence);
                SmPersistenceError::NotOwner {
                    entity: sm_session_entity(stream_id),
                }
            })?;
        self.assert_fenced_with_authority(tx, stream_id, fence, &authority)
            .await?;
        Ok(authority)
    }

    async fn assert_fenced_with_authority(
        &self,
        tx: &mut Transaction<'_>,
        stream_id: &SmSessionId,
        fence: &SmClaimFence,
        authority: &CurrentNodeIdentityGuard,
    ) -> Result<(), SmPersistenceError> {
        if !self.node_identity.owns_guard(authority) {
            return Err(SmPersistenceError::NotOwner {
                entity: sm_session_entity(stream_id),
            });
        }
        if authority.identity() != fence.owner() {
            self.remove_claim_fence_if(stream_id, fence);
            return Err(SmPersistenceError::NotOwner {
                entity: sm_session_entity(stream_id),
            });
        }
        let key = sm_session_entity_key(stream_id);
        let mut rows = tx
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
                crate::db_params![
                    key,
                    fence.owner().node_id.clone(),
                    fence.owner().node_epoch.clone(),
                    fence.epoch().0,
                ],
            )
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let held = rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
            .is_some();
        if !held {
            self.remove_claim_fence_if(stream_id, fence);
            return Err(SmPersistenceError::NotOwner {
                entity: sm_session_entity(stream_id),
            });
        }
        Ok(())
    }

    async fn guard_query(
        &self,
        sql: &str,
        params: impl crate::db::IntoParams,
    ) -> Result<crate::db::Rows, SmPersistenceError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))
    }
}

#[async_trait]
impl SmPersistenceStorage for PostgresFencedSmPersistence {
    async fn upsert_session(&self, session: PersistedSession) -> Result<(), SmPersistenceError> {
        let stream_id = session.stream_id.clone();
        let fence = self.claim_fence_for(&stream_id).await?;
        let max_resume_duration_ms = i64::try_from(session.max_resume_duration.as_millis())
            .map_err(|_| SmPersistenceError::Other("max_resume_duration overflows i64".into()))?;
        let presence_show_str = session.presence_show.as_ref().map(show_wire_str);
        let presence_payloads_xml = serialize_presence_payloads(&session.presence_payloads)?;

        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let _identity_guard = self.assert_fenced(&mut tx, &stream_id, &fence).await?;

        // Divergence (a): ignore `session.detached_at`; stamp Postgres
        // `now()` in SQL instead (both the fresh-insert VALUES and the
        // ON CONFLICT UPDATE's `excluded.detached_at_ms` read the same
        // server-computed literal, so both paths agree).
        tx.execute(
            r#"
            INSERT INTO sm_sessions (
                stream_id, user_id, full_jid, inbound_count, outbound_count,
                last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
                carbons_enabled, roster_interested, blocklist_interested, presence_available,
                presence_show, presence_status, presence_priority, replay_gap_through,
                presence_payloads
            ) VALUES (?, ?, ?, ?, ?, ?, ?, (EXTRACT(EPOCH FROM now()) * 1000)::bigint, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (stream_id) DO UPDATE SET
                user_id = excluded.user_id,
                full_jid = excluded.full_jid,
                inbound_count = excluded.inbound_count,
                outbound_count = excluded.outbound_count,
                last_acked = excluded.last_acked,
                max_resume_secs = excluded.max_resume_secs,
                detached_at_ms = excluded.detached_at_ms,
                max_resume_duration_ms = excluded.max_resume_duration_ms,
                carbons_enabled = excluded.carbons_enabled,
                roster_interested = excluded.roster_interested,
                blocklist_interested = excluded.blocklist_interested,
                presence_available = excluded.presence_available,
                presence_show = excluded.presence_show,
                presence_status = excluded.presence_status,
                presence_priority = excluded.presence_priority,
                replay_gap_through = excluded.replay_gap_through,
                presence_payloads = excluded.presence_payloads
            "#,
            crate::db_params![
                stream_id.as_str().to_string(),
                session.user_id,
                session.jid.to_string(),
                i64::from(session.inbound_count),
                i64::from(session.outbound_count),
                i64::from(session.last_acked),
                session.max_resume_time.map(i64::from),
                max_resume_duration_ms,
                i64::from(session.carbons_enabled),
                i64::from(session.roster_interested),
                i64::from(session.blocklist_interested),
                i64::from(session.presence_available),
                presence_show_str.map(str::to_string),
                session.presence_status,
                i64::from(session.presence_priority),
                session.replay_gap_through.map(i64::from),
                presence_payloads_xml,
            ],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(())
    }

    async fn get_session(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<PersistedSession>, SmPersistenceError> {
        // Read-only: no claim required to look up a session (the resume
        // path must be able to read a session it does not yet own, in
        // order to decide whether to steal it — Slice 6).
        let mut rows = self
            .guard_query(
                "SELECT stream_id, user_id, full_jid, inbound_count, outbound_count, \
                        last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, \
                        carbons_enabled, roster_interested, blocklist_interested, presence_available, \
                        presence_show, presence_status, presence_priority, replay_gap_through, \
                        presence_payloads \
                 FROM sm_sessions WHERE stream_id = ?",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            Ok(Some(decode_session(&row).map_err(|error| {
                SmPersistenceError::Corrupt {
                    stream_id: stream_id.clone(),
                    detail: error.to_string(),
                }
            })?))
        } else {
            Ok(None)
        }
    }

    async fn get_session_principal(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<AuthenticatedPrincipalRef>, SmPersistenceError> {
        let mut rows = self
            .guard_query(
                "SELECT bare_jid, CAST(auth_context_id AS TEXT), auth_context_version, principal_auth_epoch \
                 FROM sm_session_principals WHERE stream_id = ?",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?
        else {
            return Ok(None);
        };
        let bare_jid = row
            .get::<String>(0)
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?
            .parse()
            .map_err(|error| {
                SmPersistenceError::Other(format!("invalid SM principal JID: {error}"))
            })?;
        let context_id = row
            .get::<String>(1)
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?
            .parse::<uuid::Uuid>()
            .map_err(|error| {
                SmPersistenceError::Other(format!("invalid SM auth context id: {error}"))
            })?;
        let context_version = u64::try_from(
            row.get::<i64>(2)
                .map_err(|error| SmPersistenceError::Other(error.to_string()))?,
        )
        .map_err(|_| SmPersistenceError::Other("invalid SM auth context version".to_string()))?;
        let auth_epoch = u64::try_from(
            row.get::<i64>(3)
                .map_err(|error| SmPersistenceError::Other(error.to_string()))?,
        )
        .map_err(|_| SmPersistenceError::Other("invalid SM principal auth epoch".to_string()))?;
        Ok(Some(AuthenticatedPrincipalRef::new(
            bare_jid,
            AuthContextId::new(context_id),
            AuthContextVersion::new(context_version),
            PrincipalAuthEpoch::new(auth_epoch),
        )))
    }

    async fn delete_session(&self, stream_id: &SmSessionId) -> Result<(), SmPersistenceError> {
        let fence = self.claim_fence_for(stream_id).await?;
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let _identity_guard = self.assert_fenced(&mut tx, stream_id, &fence).await?;

        // Two statements rather than ON DELETE CASCADE, matching the
        // portable impl's observable lifecycle exactly.
        tx.execute(
            "DELETE FROM sm_unacked WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.execute(
            "DELETE FROM sm_session_principals WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.execute(
            "DELETE FROM sm_sessions WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        self.remove_claim_fence_if(stream_id, &fence);
        Ok(())
    }

    async fn delete_session_with_authority(
        &self,
        stream_id: &SmSessionId,
        authority: &CurrentNodeIdentityGuard,
    ) -> Result<(), SmPersistenceError> {
        let fence = self.claim_fence_for(stream_id).await?;
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        self.assert_fenced_with_authority(&mut tx, stream_id, &fence, authority)
            .await?;
        tx.execute(
            "DELETE FROM sm_unacked WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.execute(
            "DELETE FROM sm_session_principals WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.execute(
            "DELETE FROM sm_sessions WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        self.remove_claim_fence_if(stream_id, &fence);
        Ok(())
    }

    async fn quarantine_session(
        &self,
        stream_id: &SmSessionId,
        expected_fence: &SmClaimFence,
    ) -> Result<(), SmPersistenceError> {
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let _identity_guard = self
            .assert_fenced(&mut tx, stream_id, expected_fence)
            .await?;
        tx.execute(
            "DELETE FROM sm_unacked WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.execute(
            "DELETE FROM sm_session_principals WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.execute(
            "DELETE FROM sm_sessions WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        self.remove_claim_fence_if(stream_id, expected_fence);
        Ok(())
    }

    async fn append_unacked(
        &self,
        stanza: PersistedUnackedStanza,
    ) -> Result<(), SmPersistenceError> {
        let stream_id = stanza.stream_id.clone();
        let fence = self.claim_fence_for(&stream_id).await?;
        let xml = serialize_stanza(&stanza.stanza)?;
        let receipt_ms = stanza.original_receipt_at.timestamp_millis();

        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let _identity_guard = self.assert_fenced(&mut tx, &stream_id, &fence).await?;
        tx.execute(
            "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms) \
             VALUES (?, ?, ?, ?)",
            crate::db_params![
                stream_id.as_str().to_string(),
                i64::from(stanza.sequence),
                xml,
                receipt_ms,
            ],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(())
    }

    async fn ack_through(
        &self,
        stream_id: &SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, SmPersistenceError> {
        let fence = self.claim_fence_for(stream_id).await?;
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let _identity_guard = self.assert_fenced(&mut tx, stream_id, &fence).await?;
        let removed = tx
            .execute(
                "DELETE FROM sm_unacked WHERE stream_id = ? AND sequence <= ?",
                crate::db_params![stream_id.as_str().to_string(), i64::from(up_to_sequence)],
            )
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(removed)
    }

    async fn delete_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, SmPersistenceError> {
        let fence = self.claim_fence_for(stream_id).await?;
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let _identity_guard = self.assert_fenced(&mut tx, stream_id, &fence).await?;
        let mut removed = 0u64;
        for sequence in sequences {
            removed += tx
                .execute(
                    "DELETE FROM sm_unacked WHERE stream_id = ? AND sequence = ?",
                    crate::db_params![stream_id.as_str().to_string(), i64::from(*sequence)],
                )
                .await
                .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(removed)
    }

    async fn list_unacked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError> {
        // Read-only: replay on `<resumed/>` reads regardless of whether
        // this call happens to race a claim transition; the claim CAS
        // itself (Slice 6's resume path) is the actual gate on whether
        // resumption is allowed to proceed.
        let mut rows = self
            .guard_query(
                "SELECT stream_id, sequence, stanza_xml, original_receipt_at_ms \
                 FROM sm_unacked WHERE stream_id = ? \
                 ORDER BY sequence ASC",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            out.push(
                decode_unacked(&row).map_err(|error| SmPersistenceError::Corrupt {
                    stream_id: stream_id.clone(),
                    detail: error.to_string(),
                })?,
            );
        }
        Ok(out)
    }

    async fn list_expired_sessions(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        // Divergence (b): the trait's `now` parameter is advisory in this
        // impl — the window is evaluated in SQL against Postgres `now()`,
        // never the caller's clock, per element 4's "all time predicates
        // use Postgres now()" rule. This is a cross-entity listing (spans
        // every node's sessions, not just this node's claims), so it is
        // read-only and unfenced, same as the portable impl.
        let _ = now;
        let mut rows = self
            .guard_query(
                "SELECT stream_id, user_id, full_jid, inbound_count, outbound_count, \
                        last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, \
                        carbons_enabled, roster_interested, blocklist_interested, presence_available, \
                        presence_show, presence_status, presence_priority, replay_gap_through, \
                        presence_payloads \
                 FROM sm_sessions \
                 WHERE detached_at_ms + max_resume_duration_ms <= (EXTRACT(EPOCH FROM now()) * 1000)::bigint",
                (),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            out.push(decode_session(&row)?);
        }
        Ok(out)
    }

    async fn list_all_sessions(&self) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        let mut rows = self
            .guard_query(
                "SELECT stream_id, user_id, full_jid, inbound_count, outbound_count, \
                        last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, \
                        carbons_enabled, roster_interested, blocklist_interested, presence_available, \
                        presence_show, presence_status, presence_priority, replay_gap_through, \
                        presence_payloads \
                 FROM sm_sessions",
                (),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            out.push(decode_session(&row)?);
        }
        Ok(out)
    }

    // `list_all_sessions_with_unacked` is not overridden: the trait's
    // default N+1 fallback (`list_all_sessions` + `list_unacked` per
    // session) is correct here, and this impl has no divergence to apply
    // to it — the JOIN optimization the portable impl adds
    // (`joined_sessions.rs`) is a cold-start performance concern for
    // Slice 5's claim-scoped hydration, not a Slice 4 correctness
    // requirement.

    async fn store_session_atomic(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        let stream_id = session.stream_id.clone();
        let fence = self.claim_fence_for(&stream_id).await?;
        let max_resume_duration_ms = i64::try_from(session.max_resume_duration.as_millis())
            .map_err(|_| SmPersistenceError::Other("max_resume_duration overflows i64".into()))?;
        let presence_show_str = session.presence_show.as_ref().map(show_wire_str);
        let presence_payloads_xml = serialize_presence_payloads(&session.presence_payloads)?;

        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let _identity_guard = self.assert_fenced(&mut tx, &stream_id, &fence).await?;

        // Drop any pre-existing unacked rows first (see the portable
        // impl's identical comment on this statement's ordering
        // rationale), then upsert the session row (divergence (a):
        // Postgres `now()`, not `session.detached_at`), then append every
        // supplied unacked stanza.
        tx.execute(
            "DELETE FROM sm_unacked WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

        tx.execute(
            r#"
            INSERT INTO sm_sessions (
                stream_id, user_id, full_jid, inbound_count, outbound_count,
                last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
                carbons_enabled, roster_interested, blocklist_interested, presence_available,
                presence_show, presence_status, presence_priority, replay_gap_through,
                presence_payloads
            ) VALUES (?, ?, ?, ?, ?, ?, ?, (EXTRACT(EPOCH FROM now()) * 1000)::bigint, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (stream_id) DO UPDATE SET
                user_id = excluded.user_id,
                full_jid = excluded.full_jid,
                inbound_count = excluded.inbound_count,
                outbound_count = excluded.outbound_count,
                last_acked = excluded.last_acked,
                max_resume_secs = excluded.max_resume_secs,
                detached_at_ms = excluded.detached_at_ms,
                max_resume_duration_ms = excluded.max_resume_duration_ms,
                carbons_enabled = excluded.carbons_enabled,
                roster_interested = excluded.roster_interested,
                blocklist_interested = excluded.blocklist_interested,
                presence_available = excluded.presence_available,
                presence_show = excluded.presence_show,
                presence_status = excluded.presence_status,
                presence_priority = excluded.presence_priority,
                replay_gap_through = excluded.replay_gap_through,
                presence_payloads = excluded.presence_payloads
            "#,
            crate::db_params![
                stream_id.as_str().to_string(),
                session.user_id.clone(),
                session.jid.to_string(),
                i64::from(session.inbound_count),
                i64::from(session.outbound_count),
                i64::from(session.last_acked),
                session.max_resume_time.map(i64::from),
                max_resume_duration_ms,
                i64::from(session.carbons_enabled),
                i64::from(session.roster_interested),
                i64::from(session.blocklist_interested),
                i64::from(session.presence_available),
                presence_show_str.map(str::to_string),
                session.presence_status.clone(),
                i64::from(session.presence_priority),
                session.replay_gap_through.map(i64::from),
                presence_payloads_xml,
            ],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

        for stanza in &unacked {
            let xml = serialize_stanza(&stanza.stanza)?;
            let receipt_ms = stanza.original_receipt_at.timestamp_millis();
            tx.execute(
                "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms) \
                 VALUES (?, ?, ?, ?)",
                crate::db_params![
                    stream_id.as_str().to_string(),
                    i64::from(stanza.sequence),
                    xml,
                    receipt_ms,
                ],
            )
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(())
    }

    async fn store_session_atomic_with_principal(
        &self,
        principal: &AuthenticatedPrincipalRef,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        let stream_id = session.stream_id.clone();
        let fence = self.claim_fence_for(&stream_id).await?;
        let max_resume_duration_ms = i64::try_from(session.max_resume_duration.as_millis())
            .map_err(|_| SmPersistenceError::Other("max_resume_duration overflows i64".into()))?;
        let presence_show_str = session.presence_show.as_ref().map(show_wire_str);
        let presence_payloads_xml = serialize_presence_payloads(&session.presence_payloads)?;
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let _identity_guard = self.assert_fenced(&mut tx, &stream_id, &fence).await?;

        tx.execute(
            "DELETE FROM sm_unacked WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.execute(
            r#"
            INSERT INTO sm_sessions (
                stream_id, user_id, full_jid, inbound_count, outbound_count,
                last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
                carbons_enabled, roster_interested, blocklist_interested, presence_available,
                presence_show, presence_status, presence_priority, replay_gap_through,
                presence_payloads
            ) VALUES (?, ?, ?, ?, ?, ?, ?, (EXTRACT(EPOCH FROM now()) * 1000)::bigint, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (stream_id) DO UPDATE SET
                user_id = excluded.user_id, full_jid = excluded.full_jid,
                inbound_count = excluded.inbound_count, outbound_count = excluded.outbound_count,
                last_acked = excluded.last_acked, max_resume_secs = excluded.max_resume_secs,
                detached_at_ms = excluded.detached_at_ms,
                max_resume_duration_ms = excluded.max_resume_duration_ms,
                carbons_enabled = excluded.carbons_enabled,
                roster_interested = excluded.roster_interested,
                blocklist_interested = excluded.blocklist_interested,
                presence_available = excluded.presence_available,
                presence_show = excluded.presence_show,
                presence_status = excluded.presence_status,
                presence_priority = excluded.presence_priority,
                replay_gap_through = excluded.replay_gap_through,
                presence_payloads = excluded.presence_payloads
            "#,
            crate::db_params![
                stream_id.as_str().to_string(), session.user_id, session.jid.to_string(),
                i64::from(session.inbound_count), i64::from(session.outbound_count),
                i64::from(session.last_acked), session.max_resume_time.map(i64::from),
                max_resume_duration_ms, i64::from(session.carbons_enabled),
                i64::from(session.roster_interested), i64::from(session.blocklist_interested),
                i64::from(session.presence_available), presence_show_str.map(str::to_string),
                session.presence_status, i64::from(session.presence_priority),
                session.replay_gap_through.map(i64::from), presence_payloads_xml,
            ],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        tx.execute(
            "INSERT INTO sm_session_principals \
             (stream_id, bare_jid, auth_context_id, auth_context_version, principal_auth_epoch) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (stream_id) DO UPDATE SET \
                bare_jid = EXCLUDED.bare_jid, \
                auth_context_id = EXCLUDED.auth_context_id, \
                auth_context_version = EXCLUDED.auth_context_version, \
                principal_auth_epoch = EXCLUDED.principal_auth_epoch",
            crate::db_params![
                stream_id.as_str().to_string(),
                principal.bare_jid().to_string(),
                principal.auth_context_id().as_uuid().to_string(),
                i64::try_from(principal.auth_context_version().get()).map_err(|_| {
                    SmPersistenceError::Other("auth context version overflows i64".to_string())
                })?,
                i64::try_from(principal.auth_epoch().get()).map_err(|_| {
                    SmPersistenceError::Other("principal auth epoch overflows i64".to_string())
                })?,
            ],
        )
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        for stanza in &unacked {
            tx.execute(
                "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms) VALUES (?, ?, ?, ?)",
                crate::db_params![
                    stream_id.as_str().to_string(), i64::from(stanza.sequence),
                    serialize_stanza(&stanza.stanza)?, stanza.original_receipt_at.timestamp_millis(),
                ],
            )
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(())
    }

    async fn record_promotion_failure(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<u32, SmPersistenceError> {
        let fence = self.claim_fence_for(stream_id).await?;
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let _identity_guard = self.assert_fenced(&mut tx, stream_id, &fence).await?;
        let mut rows = tx
            .query(
                "UPDATE sm_sessions SET promotion_attempts = promotion_attempts + 1 \
                 WHERE stream_id = ? RETURNING promotion_attempts",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let count = match rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            Some(row) => row
                .get::<i64>(0)
                .map_err(|e| SmPersistenceError::Other(e.to_string()))?,
            None => {
                // No row for this stream_id: mirrors the portable impl's
                // `updated == 0 => Ok(0)` short-circuit.
                tx.commit()
                    .await
                    .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
                return Ok(0);
            }
        };
        tx.commit()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// ADR-0017 Phase 3 Slice 5 debt (a): the module doc's "Epoch side
    /// channel" section flagged that cells whose init failed, or whose
    /// session ends by any path other than `delete_session`, were never
    /// evicted — carried here explicitly. `InMemorySmSessionRegistry`
    /// (`session_registry/claims.rs`) now calls this on every terminal
    /// claim-ending path (`release_claim`, both `complete_claim*` success
    /// branches, `invalidate_sessions_for_jid`), so a stream_id's cached
    /// epoch never survives past the claim it was derived from — the next
    /// fenced write for that stream_id (a fresh detach + claim, or a
    /// different node's claim if this one no longer holds it) re-derives
    /// its epoch from a clean cell instead of a stale one.
    fn evict_claim_cache(&self, stream_id: &SmSessionId, expected_fence: &SmClaimFence) {
        self.remove_claim_fence_if(stream_id, expected_fence);
    }
}

#[cfg(test)]
mod tests;
