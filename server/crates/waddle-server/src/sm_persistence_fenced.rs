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
//! The database-clock detach stamp is captured on the initial insert. Refreshes
//! retain that first stamp until the source is deleted, so outbound traffic
//! cannot renew its resume window (issue #1138).
//!
//! This is a **sibling** module to [`crate::sm_persistence`], not a
//! submodule of it — "not a decorator" extends to file layout too. It
//! implements `waddle_xmpp::stream_management::persistence::
//! SmPersistenceStorage` for [`PostgresFencedSmPersistence`], a completely
//! independent type from `crate::sm_persistence::DatabaseSmPersistence`.
//! This impl shares the portable impl's `sm_sessions`, `sm_unacked`, and
//! stream-independent `sm_ingress_appends` tables. This module is
//! Postgres-only and gated behind the `clustering` Cargo feature.
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

use crate::db::Rows;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::sync::OnceCell;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ownership::{
    ClaimError, ClaimStore, CurrentNodeIdentityGuard, Entity, EntityType, SharedNodeIdentity,
};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::SmAcknowledgementMode;
use waddle_xmpp::stream_management::persistence::{
    IngressCustodyDisposition, KeyedSnapshotOutcome, PersistedDetachedAppend,
    PersistedIngressAppend, PersistedSession, PersistedUnackedStanza, PromotionAppendOutcome,
    SmAckWindow, SmClaimFence, SmPersistenceError, SmPersistenceStorage,
};

use crate::db::{Database, DatabaseDriver, Transaction};
use crate::sm_persistence::codec::{
    decode_session, decode_session_principal, decode_unacked, encode_session_principal,
    serialize_presence_payloads, serialize_stanza, show_wire_str,
};

mod atomic_store;
mod incremental;

#[cfg(test)]
mod tests;

mod implementation;

#[cfg(test)]
use implementation::remove_sm_claim_cell_if;
#[cfg(test)]
use implementation::remove_sm_claim_fence_if;
#[cfg(test)]
use implementation::sm_session_entity;

pub use implementation::PostgresFencedSmPersistence;
