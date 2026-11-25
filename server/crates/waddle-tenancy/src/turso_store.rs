//! Turso WAL-mode per-tenant storage (`<data_dir>/<tenant_id>.db`) behind a
//! bounded LRU FD pool backed by the Verus-verified `FdPoolAccounting` kernel.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use lru::LruCache;
use tokio::sync::Mutex;
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::kernels::lru_fd_pool::{AcquireAction, FdPoolAccounting, FdPoolError};
use waddle_broker_core::placement::{FenceGeneration, PlacementLeaseStore};
use waddle_broker_core::store::{
    StorageBackendKind, TenantDbHandle, TenantQueryScope, TenantStore, TenantStoreError,
};

use crate::data_ops::{
    RemoteParticipantRecord, TenantAuthIdentityRecord, TenantDataOps, TenantJobRecord,
    TenantSpaceRecord, TenantUserRecord,
};

/// Each open Turso WAL database consumes up to 3 FDs (`.db`, `-wal`, `-shm`).
pub const FDS_PER_TURSO_WAL_DB: usize = 3;

/// Default file descriptors reserved for sockets, pipes, and logs.
pub const DEFAULT_RESERVED_FDS: usize = 256;

/// Configuration for the bounded LRU FD pool backing [`TursoTenantStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TursoPoolConfig {
    /// Override `RLIMIT_NOFILE` (when `None`, reads OS `RLIMIT_NOFILE` on Linux).
    pub rlimit_nofile_override: Option<usize>,
    /// File descriptors reserved for non-DB process use.
    pub reserved_fds: usize,
}

impl Default for TursoPoolConfig {
    fn default() -> Self {
        Self {
            rlimit_nofile_override: None,
            reserved_fds: DEFAULT_RESERVED_FDS,
        }
    }
}

impl TursoPoolConfig {
    /// Compute `(rlimit_nofile, reserved_fds, capacity)` where
    /// `capacity = (rlimit_nofile - reserved_fds) / 3`.
    pub fn resolve_bounds(&self) -> Result<(usize, usize, usize), TenantStoreError> {
        let rlimit_nofile = match self.rlimit_nofile_override {
            Some(v) => v,
            None => detect_rlimit_nofile(),
        };
        if self.reserved_fds >= rlimit_nofile {
            return Err(TenantStoreError::Backend(format!(
                "reserved_fds ({}) must be < rlimit_nofile ({rlimit_nofile})",
                self.reserved_fds
            )));
        }
        let available = rlimit_nofile - self.reserved_fds;
        let capacity = available / FDS_PER_TURSO_WAL_DB;
        if capacity == 0 {
            return Err(TenantStoreError::Backend(format!(
                "insufficient FDs for Turso WAL pool: available={available}, need >= {FDS_PER_TURSO_WAL_DB}"
            )));
        }
        Ok((rlimit_nofile, self.reserved_fds, capacity))
    }
}

#[cfg(target_os = "linux")]
fn detect_rlimit_nofile() -> usize {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a valid writable `libc::rlimit` for the duration of the call.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) };
    if rc == 0 {
        usize::try_from(lim.rlim_cur).unwrap_or(1024)
    } else {
        1024
    }
}

#[cfg(not(target_os = "linux"))]
fn detect_rlimit_nofile() -> usize {
    1024
}

const TENANT_SCHEMA_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS users (
        jid TEXT PRIMARY KEY,
        account_id TEXT NOT NULL UNIQUE,
        username TEXT NOT NULL UNIQUE,
        xmpp_localpart TEXT NOT NULL UNIQUE,
        display_name TEXT
    )"#,
    r#"CREATE TRIGGER IF NOT EXISTS trg_users_account_id_immutable
       BEFORE UPDATE OF account_id ON users
       WHEN OLD.account_id != NEW.account_id
       BEGIN
           SELECT RAISE(ABORT, 'account_id is immutable');
       END"#,
    r#"CREATE TABLE IF NOT EXISTS auth_identities (
        id TEXT PRIMARY KEY,
        user_jid TEXT NOT NULL,
        provider_id TEXT NOT NULL,
        issuer TEXT NOT NULL,
        subject TEXT NOT NULL,
        UNIQUE(issuer, subject)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS spaces_metadata (
        space_node TEXT PRIMARY KEY,
        space_jid TEXT NOT NULL,
        name TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS extension_room_observers (
        plugin_id TEXT PRIMARY KEY,
        generation INTEGER NOT NULL,
        identity TEXT NOT NULL,
        scope_json TEXT NOT NULL,
        max_concurrent INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS remote_participants (
        room_id TEXT NOT NULL,
        remote_bare_jid TEXT NOT NULL,
        affiliation TEXT NOT NULL,
        PRIMARY KEY (room_id, remote_bare_jid)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS tenant_jobs (
        job_id TEXT PRIMARY KEY,
        queue TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        status TEXT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS tenant_placement_fence (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        holder_spiffe_id TEXT NOT NULL,
        holder_incarnation_pubkey BLOB NOT NULL CHECK (length(holder_incarnation_pubkey) = 32),
        generation INTEGER NOT NULL CHECK (generation >= 1),
        updated_at_ms INTEGER NOT NULL
    )"#,
];

struct PoolSlot {
    _db: turso::Database,
    conn: turso::Connection,
    pin_count: usize,
}

struct PoolInner {
    accounting: FdPoolAccounting,
    lru: LruCache<TenantId, PoolSlot>,
    evictions: usize,
}

impl PoolInner {
    fn new(
        rlimit_nofile: usize,
        reserved_fds: usize,
        capacity: usize,
    ) -> Result<Self, TenantStoreError> {
        let accounting = FdPoolAccounting::new(rlimit_nofile, reserved_fds, capacity)
            .map_err(|err| TenantStoreError::Backend(format!("invalid FD pool bounds: {err:?}")))?;
        let lru_cap = NonZeroUsize::new(capacity)
            .ok_or_else(|| TenantStoreError::Backend("LRU capacity must be >= 1".to_owned()))?;
        Ok(Self {
            accounting,
            lru: LruCache::new(lru_cap),
            evictions: 0,
        })
    }

    fn find_lru_unpinned(&self) -> Option<TenantId> {
        // `LruCache::iter()` yields MRU -> LRU, so `.rev()` yields LRU -> MRU.
        self.lru
            .iter()
            .rev()
            .find(|(_, slot)| slot.pin_count == 0)
            .map(|(tenant_id, _)| tenant_id.clone())
    }
}

/// RAII pin guard keeping a tenant's Turso connection resident while in use.
/// Decrements `pin_count` (and releases the `FdPoolAccounting` pin when hitting 0) on drop.
pub struct PinnedTursoConn {
    tenant_id: TenantId,
    conn: turso::Connection,
    pool: Arc<Mutex<PoolInner>>,
}

impl PinnedTursoConn {
    pub fn connection(&self) -> &turso::Connection {
        &self.conn
    }
}

impl Drop for PinnedTursoConn {
    fn drop(&mut self) {
        let tenant_id = self.tenant_id.clone();
        if let Ok(mut guard) = self.pool.try_lock() {
            release_pin_locked(&mut guard, &tenant_id);
        } else {
            let pool = Arc::clone(&self.pool);
            tokio::spawn(async move {
                let mut guard = pool.lock().await;
                release_pin_locked(&mut guard, &tenant_id);
            });
        }
    }
}

fn release_pin_locked(inner: &mut PoolInner, tenant_id: &TenantId) {
    if let Some(slot) = inner.lru.peek_mut(tenant_id) {
        if slot.pin_count > 0 {
            slot.pin_count -= 1;
            if slot.pin_count == 0 {
                let _ = inner.accounting.release_pin();
            }
        }
    }
}

type LocalFenceEntry = (String, [u8; 32], FenceGeneration);

/// Turso per-tenant WAL storage implementing [`TenantStore`] and [`TenantDataOps`].
pub struct TursoTenantStore {
    data_dir: PathBuf,
    pool: Arc<Mutex<PoolInner>>,
    capacity: usize,
    lease_store: Option<Arc<dyn PlacementLeaseStore>>,
    /// Tracks last verified generation per tenant for fast-path open/fence checks
    /// when no external `PlacementLeaseStore` is attached.
    local_fences: Mutex<HashMap<TenantId, LocalFenceEntry>>,
}

impl TursoTenantStore {
    pub fn new(
        data_dir: impl Into<PathBuf>,
        pool_config: TursoPoolConfig,
        lease_store: Option<Arc<dyn PlacementLeaseStore>>,
    ) -> Result<Self, TenantStoreError> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir).map_err(|err| {
            TenantStoreError::Backend(format!("failed to create Turso data dir: {err}"))
        })?;
        let (rlimit_nofile, reserved_fds, capacity) = pool_config.resolve_bounds()?;
        let inner = PoolInner::new(rlimit_nofile, reserved_fds, capacity)?;
        Ok(Self {
            data_dir,
            pool: Arc::new(Mutex::new(inner)),
            capacity,
            lease_store,
            local_fences: Mutex::new(HashMap::new()),
        })
    }

    pub const fn pool_capacity(&self) -> usize {
        self.capacity
    }

    pub async fn pool_stats(&self) -> (usize, usize, usize) {
        let guard = self.pool.lock().await;
        (
            guard.accounting.open_count,
            guard.accounting.pinned_count,
            guard.evictions,
        )
    }

    pub fn tenant_db_path(&self, tenant_id: &TenantId) -> Result<PathBuf, TenantStoreError> {
        let file_name = format!("{}.db", tenant_id.as_str());
        let path = self.data_dir.join(file_name);
        if path.parent() != Some(self.data_dir.as_path()) {
            return Err(TenantStoreError::Backend(
                "tenant DB path escaped data_dir".to_owned(),
            ));
        }
        Ok(path)
    }

    async fn open_raw_turso_db(
        path: &Path,
    ) -> Result<(turso::Database, turso::Connection), TenantStoreError> {
        let path_str = path
            .to_str()
            .ok_or_else(|| TenantStoreError::Backend("non-UTF-8 tenant DB path".to_owned()))?;
        let db = turso::Builder::new_local(path_str)
            .build()
            .await
            .map_err(|err| TenantStoreError::Backend(format!("turso open failed: {err}")))?;
        let conn = db
            .connect()
            .map_err(|err| TenantStoreError::Backend(format!("turso connect failed: {err}")))?;
        let _ = conn
            .pragma_update("journal_mode", "WAL")
            .await
            .map_err(|err| TenantStoreError::Backend(format!("WAL pragma failed: {err}")))?;
        let _ = conn
            .pragma_update("foreign_keys", "ON")
            .await
            .map_err(|err| {
                TenantStoreError::Backend(format!("foreign_keys pragma failed: {err}"))
            })?;
        Ok((db, conn))
    }

    async fn checkpoint_and_flush(conn: &turso::Connection) -> Result<(), TenantStoreError> {
        let _ = conn
            .pragma_update("wal_checkpoint", "TRUNCATE")
            .await
            .map_err(|err| {
                TenantStoreError::Backend(format!("wal_checkpoint(TRUNCATE) failed: {err}"))
            })?;
        conn.cacheflush()
            .map_err(|err| TenantStoreError::Backend(format!("cacheflush failed: {err}")))?;
        Ok(())
    }

    /// Acquire an RAII-pinned Turso connection from the bounded LRU FD pool.
    /// Evicts and checkpoints the LRU unpinned tenant DB if at capacity, or
    /// returns [`TenantStoreError::FdPoolExhausted`] if all open handles are pinned.
    pub async fn acquire_pinned_conn(
        &self,
        tenant_id: &TenantId,
    ) -> Result<PinnedTursoConn, TenantStoreError> {
        let path = self.tenant_db_path(tenant_id)?;
        if !path.exists() {
            return Err(TenantStoreError::TenantNotFound(tenant_id.clone()));
        }

        let evicted_slot = {
            let mut guard = self.pool.lock().await;
            let inner = &mut *guard;
            if let Some(slot) = inner.lru.get_mut(tenant_id) {
                if slot.pin_count == 0 {
                    inner
                        .accounting
                        .acquire_open(true)
                        .map_err(|err| match err {
                            FdPoolError::AllHandlesPinned => TenantStoreError::FdPoolExhausted {
                                limit: self.capacity,
                            },
                            other => {
                                TenantStoreError::Backend(format!("FD accounting error: {other:?}"))
                            }
                        })?;
                }
                slot.pin_count += 1;
                let conn = slot.conn.clone();
                return Ok(PinnedTursoConn {
                    tenant_id: tenant_id.clone(),
                    conn,
                    pool: Arc::clone(&self.pool),
                });
            }

            let action = guard
                .accounting
                .acquire_open(false)
                .map_err(|err| match err {
                    FdPoolError::AllHandlesPinned => TenantStoreError::FdPoolExhausted {
                        limit: self.capacity,
                    },
                    other => TenantStoreError::Backend(format!("FD accounting error: {other:?}")),
                })?;

            match action {
                AcquireAction::EvictedUnpinnedAndOpened => {
                    let victim =
                        guard
                            .find_lru_unpinned()
                            .ok_or(TenantStoreError::FdPoolExhausted {
                                limit: self.capacity,
                            })?;
                    let popped = guard.lru.pop(&victim);
                    guard.evictions += 1;
                    popped
                }
                AcquireAction::OpenedFresh | AcquireAction::ReusedOpen => None,
            }
        };

        // Checkpoint and close the evicted handle BEFORE opening the new DB so
        // open FDs never exceed the pool bound.
        if let Some(slot) = evicted_slot {
            let _ = Self::checkpoint_and_flush(&slot.conn).await;
            drop(slot);
        }

        let opened = Self::open_raw_turso_db(&path).await;
        match opened {
            Ok((db, conn)) => {
                let mut guard = self.pool.lock().await;
                let conn_clone = conn.clone();
                guard.lru.put(
                    tenant_id.clone(),
                    PoolSlot {
                        _db: db,
                        conn,
                        pin_count: 1,
                    },
                );
                Ok(PinnedTursoConn {
                    tenant_id: tenant_id.clone(),
                    conn: conn_clone,
                    pool: Arc::clone(&self.pool),
                })
            }
            Err(err) => {
                // Roll back the open+pinned reservation in FdPoolAccounting.
                let mut guard = self.pool.lock().await;
                let _ = guard.accounting.release_pin();
                let _ = guard.accounting.close_unpinned();
                Err(err)
            }
        }
    }

    async fn verify_scope_against_lease(
        &self,
        scope: &TenantQueryScope,
    ) -> Result<(), TenantStoreError> {
        if let Some(lease_store) = &self.lease_store {
            let current = lease_store
                .current(&scope.tenant_id)
                .await
                .map_err(|err| TenantStoreError::Backend(format!("lease lookup failed: {err}")))?
                .ok_or_else(|| TenantStoreError::UnauthorizedHolder(scope.tenant_id.clone()))?;
            if current.generation != scope.placement_generation {
                return Err(TenantStoreError::StalePlacementGeneration {
                    tenant_id: scope.tenant_id.clone(),
                    expected: current.generation,
                    actual: scope.placement_generation,
                });
            }
            if current.holder.spiffe_id != scope.holder.spiffe_id
                || current.holder.incarnation_pubkey != scope.holder.incarnation_pubkey
            {
                return Err(TenantStoreError::UnauthorizedHolder(
                    scope.tenant_id.clone(),
                ));
            }
        }
        Ok(())
    }

    async fn upsert_fence_row(
        conn: &turso::Connection,
        scope: &TenantQueryScope,
    ) -> Result<(), TenantStoreError> {
        let gen_i64 = i64::try_from(scope.placement_generation.get())
            .map_err(|_| TenantStoreError::Backend("placement generation overflow".to_owned()))?;
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| TenantStoreError::Backend(format!("BEGIN IMMEDIATE failed: {err}")))?;

        let existing = Self::read_fence_row(conn).await;
        match existing {
            Ok(Some((spiffe, pk, current_gen))) => {
                if scope.placement_generation < current_gen {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(TenantStoreError::StalePlacementGeneration {
                        tenant_id: scope.tenant_id.clone(),
                        expected: current_gen,
                        actual: scope.placement_generation,
                    });
                }
                if scope.placement_generation == current_gen
                    && (spiffe != scope.holder.spiffe_id.as_str()
                        || pk.as_slice() != scope.holder.incarnation_pubkey.as_bytes())
                {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(TenantStoreError::UnauthorizedHolder(
                        scope.tenant_id.clone(),
                    ));
                }
                let res = conn
                    .execute(
                        "UPDATE tenant_placement_fence
                         SET holder_spiffe_id = ?1,
                             holder_incarnation_pubkey = ?2,
                             generation = ?3,
                             updated_at_ms = CAST(unixepoch('subsec') * 1000 AS INTEGER)
                         WHERE singleton = 1 AND generation <= ?3",
                        (
                            scope.holder.spiffe_id.as_str(),
                            scope.holder.incarnation_pubkey.as_bytes().to_vec(),
                            gen_i64,
                        ),
                    )
                    .await;
                if let Err(err) = res {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(TenantStoreError::Backend(format!(
                        "fence update failed: {err}"
                    )));
                }
            }
            Ok(None) => {
                let res = conn
                    .execute(
                        "INSERT INTO tenant_placement_fence
                         (singleton, holder_spiffe_id, holder_incarnation_pubkey, generation, updated_at_ms)
                         VALUES (1, ?1, ?2, ?3, CAST(unixepoch('subsec') * 1000 AS INTEGER))",
                        (
                            scope.holder.spiffe_id.as_str(),
                            scope.holder.incarnation_pubkey.as_bytes().to_vec(),
                            gen_i64,
                        ),
                    )
                    .await;
                if let Err(err) = res {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(TenantStoreError::Backend(format!(
                        "fence insert failed: {err}"
                    )));
                }
            }
            Err(err) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                return Err(err);
            }
        }

        conn.execute("COMMIT", ())
            .await
            .map_err(|err| TenantStoreError::Backend(format!("fence COMMIT failed: {err}")))?;
        Ok(())
    }

    async fn read_fence_row(
        conn: &turso::Connection,
    ) -> Result<Option<(String, Vec<u8>, FenceGeneration)>, TenantStoreError> {
        let mut rows = conn
            .query(
                "SELECT holder_spiffe_id, holder_incarnation_pubkey, generation
                 FROM tenant_placement_fence WHERE singleton = 1",
                (),
            )
            .await
            .map_err(|err| TenantStoreError::Backend(format!("fence query failed: {err}")))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|err| TenantStoreError::Backend(format!("fence row read failed: {err}")))?
        {
            let spiffe: String = row
                .get(0)
                .map_err(|err| TenantStoreError::Backend(format!("fence col0: {err}")))?;
            let pk: Vec<u8> = row
                .get(1)
                .map_err(|err| TenantStoreError::Backend(format!("fence col1: {err}")))?;
            let gen_i64: i64 = row
                .get(2)
                .map_err(|err| TenantStoreError::Backend(format!("fence col2: {err}")))?;
            let gen = u64::try_from(gen_i64).unwrap_or(0);
            Ok(Some((spiffe, pk, FenceGeneration::new(gen))))
        } else {
            Ok(None)
        }
    }

    async fn check_fence_on_conn(
        conn: &turso::Connection,
        scope: &TenantQueryScope,
    ) -> Result<(), TenantStoreError> {
        let Some((spiffe, pk, current_gen)) = Self::read_fence_row(conn).await? else {
            return Err(TenantStoreError::UnauthorizedHolder(
                scope.tenant_id.clone(),
            ));
        };
        if current_gen != scope.placement_generation {
            return Err(TenantStoreError::StalePlacementGeneration {
                tenant_id: scope.tenant_id.clone(),
                expected: current_gen,
                actual: scope.placement_generation,
            });
        }
        if spiffe != scope.holder.spiffe_id.as_str()
            || pk.as_slice() != scope.holder.incarnation_pubkey.as_bytes()
        {
            return Err(TenantStoreError::UnauthorizedHolder(
                scope.tenant_id.clone(),
            ));
        }
        Ok(())
    }

    async fn ensure_fenced_conn(
        &self,
        scope: &TenantQueryScope,
    ) -> Result<PinnedTursoConn, TenantStoreError> {
        self.verify_scope_against_lease(scope).await?;
        let pinned = self.acquire_pinned_conn(&scope.tenant_id).await?;
        Self::check_fence_on_conn(pinned.connection(), scope).await?;
        Ok(pinned)
    }
}

#[async_trait]
impl TenantStore for TursoTenantStore {
    fn backend_kind(&self) -> StorageBackendKind {
        StorageBackendKind::TursoWal
    }

    async fn provision_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError> {
        let path = self.tenant_db_path(tenant_id)?;
        if path.exists() {
            return Err(TenantStoreError::TenantAlreadyExists(tenant_id.clone()));
        }
        let (db, conn) = Self::open_raw_turso_db(&path).await?;
        for stmt in TENANT_SCHEMA_STATEMENTS {
            conn.execute(stmt, ()).await.map_err(|err| {
                TenantStoreError::Backend(format!("tenant schema DDL failed: {err}"))
            })?;
        }
        Self::checkpoint_and_flush(&conn).await?;
        drop(conn);
        drop(db);
        Ok(())
    }

    async fn open_tenant(
        &self,
        scope: &TenantQueryScope,
    ) -> Result<TenantDbHandle, TenantStoreError> {
        self.verify_scope_against_lease(scope).await?;
        let pinned = self.acquire_pinned_conn(&scope.tenant_id).await?;
        Self::upsert_fence_row(pinned.connection(), scope).await?;
        {
            let mut fences = self.local_fences.lock().await;
            fences.insert(
                scope.tenant_id.clone(),
                (
                    scope.holder.spiffe_id.as_str().to_owned(),
                    *scope.holder.incarnation_pubkey.as_bytes(),
                    scope.placement_generation,
                ),
            );
        }
        Ok(TenantDbHandle {
            tenant_id: scope.tenant_id.clone(),
            backend: StorageBackendKind::TursoWal,
            placement_generation: scope.placement_generation,
        })
    }

    async fn verify_write_fence(&self, scope: &TenantQueryScope) -> Result<(), TenantStoreError> {
        let _pinned = self.ensure_fenced_conn(scope).await?;
        Ok(())
    }

    async fn checkpoint_and_release(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError> {
        let popped = {
            let mut guard = self.pool.lock().await;
            if let Some(slot) = guard.lru.peek(tenant_id) {
                if slot.pin_count > 0 {
                    return Err(TenantStoreError::Backend(format!(
                        "cannot release pinned tenant handle for {tenant_id}"
                    )));
                }
            }
            let slot = guard.lru.pop(tenant_id);
            if slot.is_some() {
                let _ = guard.accounting.close_unpinned();
            }
            slot
        };
        if let Some(slot) = popped {
            Self::checkpoint_and_flush(&slot.conn).await?;
            drop(slot);
        }
        Ok(())
    }

    async fn delete_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError> {
        self.checkpoint_and_release(tenant_id).await?;
        self.local_fences.lock().await.remove(tenant_id);
        let db_path = self.tenant_db_path(tenant_id)?;
        let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
        let shm_path = PathBuf::from(format!("{}-shm", db_path.display()));
        for candidate in [db_path, wal_path, shm_path] {
            if candidate.exists() {
                std::fs::remove_file(&candidate).map_err(|err| {
                    TenantStoreError::Backend(format!(
                        "failed to remove {}: {err}",
                        candidate.display()
                    ))
                })?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl TenantDataOps for TursoTenantStore {
    async fn upsert_user(
        &self,
        scope: &TenantQueryScope,
        user: &TenantUserRecord,
    ) -> Result<(), TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        pinned
            .connection()
            .execute(
                "INSERT INTO users (jid, account_id, username, xmpp_localpart, display_name)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(jid) DO UPDATE SET
                     username = excluded.username,
                     xmpp_localpart = excluded.xmpp_localpart,
                     display_name = excluded.display_name",
                (
                    user.jid.as_str(),
                    user.account_id.to_string(),
                    user.username.as_str(),
                    user.xmpp_localpart.as_str(),
                    user.display_name.as_deref(),
                ),
            )
            .await
            .map_err(|err| TenantStoreError::Backend(format!("upsert_user failed: {err}")))?;
        Ok(())
    }

    async fn get_user_by_localpart(
        &self,
        scope: &TenantQueryScope,
        xmpp_localpart: &str,
    ) -> Result<Option<TenantUserRecord>, TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        let mut rows = pinned
            .connection()
            .query(
                "SELECT jid, account_id, username, xmpp_localpart, display_name
                 FROM users WHERE xmpp_localpart = ?1",
                [xmpp_localpart],
            )
            .await
            .map_err(|err| TenantStoreError::Backend(format!("get_user failed: {err}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| TenantStoreError::Backend(format!("get_user row: {err}")))?
        else {
            return Ok(None);
        };
        let jid: String = row
            .get(0)
            .map_err(|e| TenantStoreError::Backend(e.to_string()))?;
        let account_id_str: String = row
            .get(1)
            .map_err(|e| TenantStoreError::Backend(e.to_string()))?;
        let username: String = row
            .get(2)
            .map_err(|e| TenantStoreError::Backend(e.to_string()))?;
        let localpart: String = row
            .get(3)
            .map_err(|e| TenantStoreError::Backend(e.to_string()))?;
        let display_name: Option<String> = row
            .get(4)
            .map_err(|e| TenantStoreError::Backend(e.to_string()))?;
        let uuid = Uuid::parse_str(&account_id_str)
            .map_err(|e| TenantStoreError::Backend(format!("invalid account_id uuid: {e}")))?;
        Ok(Some(TenantUserRecord {
            account_id: AccountId::from_uuid(uuid),
            jid,
            username,
            xmpp_localpart: localpart,
            display_name,
        }))
    }

    async fn update_user_account_id_for_test(
        &self,
        scope: &TenantQueryScope,
        jid: &str,
        new_account_id: AccountId,
    ) -> Result<(), TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        pinned
            .connection()
            .execute(
                "UPDATE users SET account_id = ?1 WHERE jid = ?2",
                (new_account_id.to_string(), jid),
            )
            .await
            .map_err(|err| TenantStoreError::Backend(format!("update account_id failed: {err}")))?;
        Ok(())
    }

    async fn upsert_auth_identity(
        &self,
        scope: &TenantQueryScope,
        identity: &TenantAuthIdentityRecord,
    ) -> Result<(), TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        pinned
            .connection()
            .execute(
                "INSERT INTO auth_identities (id, user_jid, provider_id, issuer, subject)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(issuer, subject) DO UPDATE SET
                     user_jid = excluded.user_jid,
                     provider_id = excluded.provider_id",
                (
                    identity.id.as_str(),
                    identity.user_jid.as_str(),
                    identity.provider_id.as_str(),
                    identity.issuer.as_str(),
                    identity.subject.as_str(),
                ),
            )
            .await
            .map_err(|err| {
                TenantStoreError::Backend(format!("upsert_auth_identity failed: {err}"))
            })?;
        Ok(())
    }

    async fn get_auth_identity(
        &self,
        scope: &TenantQueryScope,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<TenantAuthIdentityRecord>, TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        let mut rows = pinned
            .connection()
            .query(
                "SELECT id, user_jid, provider_id, issuer, subject
                 FROM auth_identities WHERE issuer = ?1 AND subject = ?2",
                (issuer, subject),
            )
            .await
            .map_err(|err| TenantStoreError::Backend(format!("get_auth_identity failed: {err}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| TenantStoreError::Backend(format!("get_auth_identity row: {err}")))?
        else {
            return Ok(None);
        };
        Ok(Some(TenantAuthIdentityRecord {
            id: row
                .get(0)
                .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
            user_jid: row
                .get(1)
                .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
            provider_id: row
                .get(2)
                .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
            issuer: row
                .get(3)
                .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
            subject: row
                .get(4)
                .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
        }))
    }

    async fn upsert_space(
        &self,
        scope: &TenantQueryScope,
        space: &TenantSpaceRecord,
    ) -> Result<(), TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        pinned
            .connection()
            .execute(
                "INSERT INTO spaces_metadata (space_node, space_jid, name)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(space_node) DO UPDATE SET
                     space_jid = excluded.space_jid,
                     name = excluded.name",
                (
                    space.space_node.as_str(),
                    space.space_jid.as_str(),
                    space.name.as_str(),
                ),
            )
            .await
            .map_err(|err| TenantStoreError::Backend(format!("upsert_space failed: {err}")))?;
        Ok(())
    }

    async fn get_space(
        &self,
        scope: &TenantQueryScope,
        space_node: &str,
    ) -> Result<Option<TenantSpaceRecord>, TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        let mut rows = pinned
            .connection()
            .query(
                "SELECT space_node, space_jid, name FROM spaces_metadata WHERE space_node = ?1",
                [space_node],
            )
            .await
            .map_err(|err| TenantStoreError::Backend(format!("get_space failed: {err}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| TenantStoreError::Backend(format!("get_space row: {err}")))?
        else {
            return Ok(None);
        };
        Ok(Some(TenantSpaceRecord {
            space_node: row
                .get(0)
                .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
            space_jid: row
                .get(1)
                .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
            name: row
                .get(2)
                .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
        }))
    }

    async fn record_remote_participant(
        &self,
        scope: &TenantQueryScope,
        participant: &RemoteParticipantRecord,
    ) -> Result<(), TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        pinned
            .connection()
            .execute(
                "INSERT INTO remote_participants (room_id, remote_bare_jid, affiliation)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(room_id, remote_bare_jid) DO UPDATE SET
                     affiliation = excluded.affiliation",
                (
                    participant.room_id.as_str(),
                    participant.remote_bare_jid.as_str(),
                    participant.affiliation.as_str(),
                ),
            )
            .await
            .map_err(|err| {
                TenantStoreError::Backend(format!("record_remote_participant failed: {err}"))
            })?;
        Ok(())
    }

    async fn list_remote_participants(
        &self,
        scope: &TenantQueryScope,
        room_id: &str,
    ) -> Result<Vec<RemoteParticipantRecord>, TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        let mut rows = pinned
            .connection()
            .query(
                "SELECT room_id, remote_bare_jid, affiliation
                 FROM remote_participants WHERE room_id = ?1 ORDER BY remote_bare_jid",
                [room_id],
            )
            .await
            .map_err(|err| {
                TenantStoreError::Backend(format!("list_remote_participants failed: {err}"))
            })?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|err| TenantStoreError::Backend(format!("list_remote row: {err}")))?
        {
            out.push(RemoteParticipantRecord {
                room_id: row
                    .get(0)
                    .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
                remote_bare_jid: row
                    .get(1)
                    .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
                affiliation: row
                    .get(2)
                    .map_err(|e| TenantStoreError::Backend(e.to_string()))?,
            });
        }
        Ok(out)
    }

    async fn enqueue_job(
        &self,
        scope: &TenantQueryScope,
        job: &TenantJobRecord,
    ) -> Result<(), TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        pinned
            .connection()
            .execute(
                "INSERT INTO tenant_jobs (job_id, queue, payload_json, status)
                 VALUES (?1, ?2, ?3, ?4)",
                (
                    job.job_id.as_str(),
                    job.queue.as_str(),
                    job.payload_json.as_str(),
                    job.status.as_str(),
                ),
            )
            .await
            .map_err(|err| TenantStoreError::Backend(format!("enqueue_job failed: {err}")))?;
        Ok(())
    }

    async fn claim_next_job(
        &self,
        scope: &TenantQueryScope,
        queue: &str,
    ) -> Result<Option<TenantJobRecord>, TenantStoreError> {
        let pinned = self.ensure_fenced_conn(scope).await?;
        let mut rows = pinned
            .connection()
            .query(
                "SELECT job_id, queue, payload_json, status
                 FROM tenant_jobs
                 WHERE queue = ?1 AND status = 'pending'
                 ORDER BY job_id LIMIT 1",
                [queue],
            )
            .await
            .map_err(|err| TenantStoreError::Backend(format!("claim_next_job query: {err}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| TenantStoreError::Backend(format!("claim_next_job row: {err}")))?
        else {
            return Ok(None);
        };
        let job_id: String = row
            .get(0)
            .map_err(|e| TenantStoreError::Backend(e.to_string()))?;
        let queue_val: String = row
            .get(1)
            .map_err(|e| TenantStoreError::Backend(e.to_string()))?;
        let payload_json: String = row
            .get(2)
            .map_err(|e| TenantStoreError::Backend(e.to_string()))?;
        pinned
            .connection()
            .execute(
                "UPDATE tenant_jobs SET status = 'claimed' WHERE job_id = ?1 AND status = 'pending'",
                [job_id.as_str()],
            )
            .await
            .map_err(|err| TenantStoreError::Backend(format!("claim_next_job update: {err}")))?;
        Ok(Some(TenantJobRecord {
            job_id,
            queue: queue_val,
            payload_json,
            status: "claimed".to_owned(),
        }))
    }
}
