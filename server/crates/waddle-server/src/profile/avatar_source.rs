//! `user_avatar_source` provenance — the table that distinguishes
//! OIDC-managed avatars (which the bridge owns and may overwrite /
//! remove on re-login) from user-self-published avatars (which it
//! must NOT touch).
//!
//! Two surfaces:
//!
//! - [`read_avatar_source`] — the OIDC publish chain consults this
//!   on `RemoveIfOidcOwned` and suppresses the empty-`<metadata/>`
//!   publish when the row says `'user'`.
//! - [`record_self_published`] / [`record_user_retracted`] — the
//!   WebSocket PEP-publish handler updates this after a user
//!   wire-publish to their own avatar / vCard4 photo node.

use std::sync::Arc;

use dashmap::DashMap;
use jid::BareJid;
use kameo::actor::ActorRef;
use thiserror::Error;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::warn;

use crate::db::actor::{DbActor, DbExecute, DbQueryOne};
use crate::server::routes::websocket::WebSocketState;

/// Outcome of the user-managed avatar guard query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarSource {
    /// `source = 'oidc'` (the default for fresh OIDC publishes). The
    /// OIDC bridge owns the avatar.
    Oidc,
    /// `source = 'user'`. The user has self-published via wire
    /// XEP-0084 and the bridge MUST NOT overwrite or remove it.
    User,
    /// No row matched. Means the user has neither been touched by
    /// the OIDC bridge nor wire-published an avatar yet — the
    /// guard's downstream idempotence check (was there ever an
    /// avatar?) handles this safely.
    Unknown,
}

impl AvatarSource {
    fn parse(raw: &str) -> Self {
        match raw {
            "user" => AvatarSource::User,
            "oidc" => AvatarSource::Oidc,
            _ => AvatarSource::Unknown,
        }
    }
}

/// Typed error from the avatar-source storage helpers. The single
/// variant wraps the kameo actor error string (which already
/// Display-formats the inner DB error) — kameo's `ask` reply is
/// flattened, so a deeper typed wrapper isn't reachable here without
/// re-introducing string-formatting at the actor seam.
#[derive(Debug, Error)]
pub enum AvatarSourceStorageError {
    #[error("avatar_source storage failure: {0}")]
    Storage(String),
}

/// Read the provenance flag for the user owning `jid`. Returns
/// `Unknown` when the row is missing or `jid` has no localpart
/// (server-domain-only JIDs can't own avatars).
pub async fn read_avatar_source(
    db_actor: &ActorRef<DbActor>,
    jid: &BareJid,
) -> Result<AvatarSource, AvatarSourceStorageError> {
    let Some(localpart) = jid.node().map(|n| n.as_str().to_string()) else {
        // No localpart → no possible avatar owner; skip the DB
        // round-trip. Mirrors `record_self_published`'s early return.
        return Ok(AvatarSource::Unknown);
    };
    let row = db_actor
        .ask(DbQueryOne {
            sql: "SELECT source FROM user_avatar_source WHERE xmpp_localpart = ? LIMIT 1"
                .to_string(),
            params: vec![localpart.into()],
        })
        .await
        .map_err(|e| AvatarSourceStorageError::Storage(e.to_string()))?;
    let Some(row) = row else {
        return Ok(AvatarSource::Unknown);
    };
    let raw: Option<String> = row.first().and_then(|v| match v {
        crate::db::Value::Text(s) => Some(s.clone()),
        _ => None,
    });
    Ok(raw
        .as_deref()
        .map(AvatarSource::parse)
        .unwrap_or(AvatarSource::Unknown))
}

async fn upsert_source(
    db_actor: &ActorRef<DbActor>,
    localpart: &str,
    source: &str,
) -> Result<(), AvatarSourceStorageError> {
    let now = chrono::Utc::now().to_rfc3339();
    db_actor
        .ask(DbExecute {
            sql: r#"
                INSERT INTO user_avatar_source (xmpp_localpart, source, updated_at)
                VALUES (?, ?, ?)
                ON CONFLICT(xmpp_localpart) DO UPDATE
                  SET source = excluded.source, updated_at = excluded.updated_at
            "#
            .to_string(),
            params: vec![localpart.into(), source.into(), now.into()],
        })
        .await
        .map_err(|e| AvatarSourceStorageError::Storage(e.to_string()))?;
    Ok(())
}

/// Per-`BareJid` mutex set guarding the `user_avatar_source`
/// read-then-publish critical section. Both the OIDC publish chain
/// and the wire avatar-publish hook acquire the same mutex by
/// `BareJid` so they cannot race.
///
/// Entries are inserted on first acquire and **removed when the last
/// in-flight guard for that JID drops**, so the map stays bounded by
/// the number of *currently-contended* JIDs rather than growing to
/// the lifetime set of users ever seen. See [`AvatarLockGuard::drop`]
/// for the eviction protocol.
#[derive(Debug, Default)]
pub struct AvatarLockMap {
    inner: DashMap<BareJid, Arc<Mutex<()>>>,
}

impl AvatarLockMap {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    /// Number of JIDs currently holding or waiting on an entry.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Acquire the per-(`BareJid`) avatar-source mutex against `self`,
    /// returning an [`AvatarLockGuard`] that evicts the map entry on
    /// drop when no other acquirer is in flight. Public so call sites
    /// holding an `Arc<AvatarLockMap>` directly (e.g. tests) can use
    /// the same eviction-aware path as
    /// [`acquire_per_jid_lock`].
    pub async fn acquire(self: &Arc<Self>, jid: &BareJid) -> AvatarLockGuard {
        let mutex = self
            .inner
            .entry(jid.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let guard = mutex.lock_owned().await;
        AvatarLockGuard {
            guard: Some(guard),
            map: Arc::clone(self),
            jid: jid.clone(),
        }
    }
}

/// Owned guard returned by [`acquire_per_jid_lock`]. On drop, releases
/// the per-JID mutex AND evicts the map entry iff no other acquirer
/// is currently holding or waiting on it (detected via
/// `Arc::strong_count == 1`, i.e. the map's own reference).
pub struct AvatarLockGuard {
    // Field order matters: `guard` drops before `map`/`jid`, so the
    // mutex is released and the per-acquirer `Arc<Mutex<()>>` clone is
    // dropped *before* we attempt the strong-count check in our own
    // Drop impl below.
    guard: Option<OwnedMutexGuard<()>>,
    map: Arc<AvatarLockMap>,
    jid: BareJid,
}

impl Drop for AvatarLockGuard {
    fn drop(&mut self) {
        // Release the mutex + the acquirer's `Arc<Mutex<()>>` clone first.
        self.guard.take();

        // `remove_if` holds the shard write lock for `self.jid` during
        // the callback, so no concurrent `acquire` can observe / clone
        // the Arc between our strong-count read and the removal. While
        // we hold that lock:
        //
        // - `strong_count == 1` → only the map holds the Arc; nobody
        //   else is acquiring or waiting → safe to evict.
        // - `strong_count > 1`  → at least one other acquirer cloned
        //   the Arc out of the map; keep the entry so they continue to
        //   serialize through the same mutex.
        self.map
            .inner
            .remove_if(&self.jid, |_, mutex| Arc::strong_count(mutex) == 1);
    }
}

/// Acquire the per-(`BareJid`) avatar-source mutex. Both the OIDC
/// publish chain and the wire avatar-publish hook take this lock;
/// holding it across read+write closes the TOCTOU race where OIDC
/// could read `'oidc'` between a user's wire publish and the
/// downstream `record_self_published` flip and then wipe the
/// just-set avatar.
///
/// Returns an [`AvatarLockGuard`] so the caller can hold it across
/// further async operations without lifetime gymnastics. The guard's
/// `Drop` evicts the map entry when no other acquirer is in flight.
pub async fn acquire_per_jid_lock(state: &WebSocketState, jid: &BareJid) -> AvatarLockGuard {
    state.deps.protocol.avatar_source_locks.acquire(jid).await
}

/// Mark `jid`'s avatar as user-self-published. Idempotent — repeated
/// calls just re-confirm `'user'`. Logs and swallows errors (the
/// publish itself already succeeded; failing the IQ because we
/// couldn't update a provenance flag would be worse).
pub async fn record_self_published(db_actor: &ActorRef<DbActor>, jid: &BareJid) {
    let Some(localpart) = jid.node().map(|n| n.as_str().to_string()) else {
        return;
    };
    if let Err(error) = upsert_source(db_actor, &localpart, "user").await {
        warn!(
            jid = %jid,
            error = %error,
            "Failed to mark avatar_source='user'; OIDC reconcile may still overwrite"
        );
    }
}

/// Mark `jid`'s avatar as OIDC-managed. Called when the user
/// explicitly opts back into OIDC management (e.g. by wire-publishing
/// the XEP-0084 §4.3 empty-`<metadata/>` removal shape themselves —
/// "I'm not picking my own avatar anymore, you can manage it") AND
/// after the OIDC bridge successfully publishes a new avatar (so the
/// flag tracks current ownership intent).
pub async fn record_oidc_managed(db_actor: &ActorRef<DbActor>, jid: &BareJid) {
    let Some(localpart) = jid.node().map(|n| n.as_str().to_string()) else {
        return;
    };
    if let Err(error) = upsert_source(db_actor, &localpart, "oidc").await {
        warn!(
            jid = %jid,
            error = %error,
            "Failed to mark avatar_source='oidc'; provenance state may be stale"
        );
    }
}

#[cfg(test)]
mod lock_map_tests {
    use super::*;

    fn jid(local: &str) -> BareJid {
        format!("{local}@waddle.test").parse().expect("valid jid")
    }

    #[tokio::test]
    async fn entry_is_evicted_after_solo_guard_drops() {
        let map = Arc::new(AvatarLockMap::new());
        let alice = jid("alice");
        {
            let _guard = map.acquire(&alice).await;
            assert_eq!(map.len(), 1, "entry exists while guard is held");
        }
        assert_eq!(
            map.len(),
            0,
            "entry MUST be evicted once the last guard drops"
        );
    }

    #[tokio::test]
    async fn entry_survives_while_a_second_acquirer_is_waiting() {
        let map = Arc::new(AvatarLockMap::new());
        let alice = jid("alice");

        let first = map.acquire(&alice).await;
        assert_eq!(map.len(), 1);

        // Second acquire blocks on the held mutex, but the map entry must
        // remain because two acquirers are now sharing the same Arc.
        let map_for_task = Arc::clone(&map);
        let alice_for_task = alice.clone();
        let task = tokio::spawn(async move { map_for_task.acquire(&alice_for_task).await });

        // Give the task a chance to clone the Arc out of the map and
        // start waiting on the mutex.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        drop(first);
        // First-guard drop must NOT evict — the spawned task is still
        // waiting on the same mutex.
        let second = task.await.expect("spawned acquire panicked");
        assert_eq!(
            map.len(),
            1,
            "entry MUST survive while a follower is still holding the guard"
        );
        drop(second);
        assert_eq!(
            map.len(),
            0,
            "entry MUST be evicted once the last follower drops"
        );
    }

    #[tokio::test]
    async fn distinct_jids_do_not_share_eviction() {
        let map = Arc::new(AvatarLockMap::new());
        let alice = jid("alice");
        let bob = jid("bob");

        let alice_guard = map.acquire(&alice).await;
        let bob_guard = map.acquire(&bob).await;
        assert_eq!(map.len(), 2);

        drop(alice_guard);
        assert_eq!(map.len(), 1, "dropping alice MUST NOT evict bob");
        drop(bob_guard);
        assert_eq!(map.len(), 0);
    }
}
