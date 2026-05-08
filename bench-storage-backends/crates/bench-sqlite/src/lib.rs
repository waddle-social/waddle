//! SQLite backend for the stanza-database benchmark.
//!
//! Two backing modes:
//!
//! * [`SqliteBacking::Disk`] - file on disk, WAL mode. This is the mode that
//!   matches waddle's current production setup.
//! * [`SqliteBacking::Memory`] - shared-cache in-memory DB. A flusher thread
//!   uses SQLite's online-backup API to snapshot pages to a disk file every
//!   `flush_interval`. Trades durability (up to `flush_interval` of write
//!   loss on crash) for near-RAM write speed.
//!
//! Both modes use:
//! * **One dedicated writer thread** fed via `std::sync::mpsc` - avoids
//!   `SQLITE_BUSY` thrash under high write contention. Disk-mode uses
//!   `synchronous=NORMAL`; memory-mode has no fsync cost on the hot path.
//! * **r2d2 reader pool** for the hot read path. WAL gives concurrent readers
//!   on disk; shared-cache URI gives concurrent readers on memory.
//!
//! Schema matches
//! `waddle/server/crates/waddle-xmpp/src/mam/storage.rs:226-271` byte-for-byte.

mod backing;
mod flusher;
mod query;
mod schema;
mod writer;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use async_trait::async_trait;
use bench_core::message::ArchivedMessage;
use bench_core::metrics::{op_stats_from_hist, OpStats};
use bench_core::store::{StanzaStore, StoreError};
use hdrhistogram::Histogram;
use r2d2::Pool;
use rusqlite::Connection;

pub use backing::SqliteBacking;

use flusher::flusher_loop;
use query::run_query;
use schema::{apply_pragmas, UriManager, MAM_SCHEMA};
use writer::{writer_loop, WriteJob};

pub struct SqliteStore {
    backing: SqliteBacking,
    writer_tx: Mutex<Option<mpsc::SyncSender<WriteJob>>>,
    pool: Pool<UriManager>,
    writer_thread: Mutex<Option<thread::JoinHandle<()>>>,
    /// Only populated in `Memory` mode.
    flusher_thread: Mutex<Option<thread::JoinHandle<()>>>,
    flusher_shutdown: Arc<AtomicBool>,
    /// Keep-alive connection for shared-cache memory DBs: if *all* connections
    /// close, the DB disappears. None in disk mode.
    _keepalive: Option<Mutex<Connection>>,
    /// Time a write job spent in the channel between the caller stamping
    /// it and the writer thread picking it up. Pure queueing delay.
    queue_wait_ns: Arc<Mutex<Histogram<u64>>>,
    /// Time the writer thread spent actually running the INSERT + commit,
    /// excluding queue wait. Pure SQL-execute cost.
    sql_exec_ns: Arc<Mutex<Histogram<u64>>>,
    /// Time each flush took end-to-end (memory mode only).
    flush_latency_ns: Arc<Mutex<Histogram<u64>>>,
}

impl SqliteStore {
    /// Open a disk-backed store (legacy / default).
    pub fn open(path: impl Into<PathBuf>, reader_pool_size: u32) -> Result<Arc<Self>, StoreError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(StoreError::backend)?;
        }
        Self::open_with_backing(SqliteBacking::Disk(path), reader_pool_size)
    }

    /// Open a store with an explicit backing mode.
    pub fn open_with_backing(
        backing: SqliteBacking,
        reader_pool_size: u32,
    ) -> Result<Arc<Self>, StoreError> {
        // Ensure snapshot parent dir exists for memory mode.
        if let SqliteBacking::Memory { snapshot_to, .. } = &backing {
            if let Some(parent) = snapshot_to.parent() {
                std::fs::create_dir_all(parent).map_err(StoreError::backend)?;
            }
        }

        let uri = backing.connection_uri();
        let flags = backing.open_flags();

        // For shared-cache memory: open a keepalive connection BEFORE the
        // pool, so the DB exists when the pool tries to connect.
        let keepalive = if matches!(backing, SqliteBacking::Memory { .. }) {
            let c = Connection::open_with_flags(&uri, flags).map_err(StoreError::backend)?;
            apply_pragmas(&c, &backing).map_err(StoreError::backend)?;
            // Also create the schema on the keepalive so pool connections see it
            // even if they connect before the writer has initialised.
            c.execute_batch(MAM_SCHEMA).map_err(StoreError::backend)?;
            Some(Mutex::new(c))
        } else {
            None
        };

        let manager = UriManager {
            uri: uri.clone(),
            flags,
            backing: backing.clone(),
        };
        let pool = Pool::builder()
            .max_size(reader_pool_size)
            .build(manager)
            .map_err(StoreError::backend)?;

        // Histograms shared with the writer thread. 1ns..60s, 3 sig figs.
        let new_hist = || {
            Arc::new(Mutex::new(
                Histogram::<u64>::new_with_bounds(1, 60 * 1_000_000_000, 3).unwrap(),
            ))
        };
        let queue_wait = new_hist();
        let sql_exec = new_hist();
        let flush_latency = new_hist();

        // Writer channel + thread.
        let (tx, rx) = mpsc::sync_channel::<WriteJob>(4096);
        let writer_uri = uri.clone();
        let writer_flags = flags;
        let writer_backing = backing.clone();
        let qw = queue_wait.clone();
        let se = sql_exec.clone();
        let writer_thread = thread::Builder::new()
            .name("bench-sqlite-writer".into())
            .spawn(move || writer_loop(writer_uri, writer_flags, writer_backing, rx, qw, se))
            .map_err(StoreError::backend)?;

        // Flusher thread: only for memory backing.
        let flusher_shutdown = Arc::new(AtomicBool::new(false));
        let flush_count = Arc::new(AtomicU64::new(0));
        let flusher_thread = if let SqliteBacking::Memory {
            snapshot_to,
            flush_interval,
            ..
        } = &backing
        {
            let src_uri = uri.clone();
            let src_flags = flags;
            let src_backing = backing.clone();
            let dst_path = snapshot_to.clone();
            let interval = *flush_interval;
            let shutdown = flusher_shutdown.clone();
            let fl = flush_latency.clone();
            let fc = flush_count.clone();
            let handle = thread::Builder::new()
                .name("bench-sqlite-flusher".into())
                .spawn(move || {
                    flusher_loop(
                        src_uri,
                        src_flags,
                        src_backing,
                        dst_path,
                        interval,
                        shutdown,
                        fl,
                        fc,
                    )
                })
                .map_err(StoreError::backend)?;
            Some(handle)
        } else {
            None
        };

        Ok(Arc::new(Self {
            backing,
            writer_tx: Mutex::new(Some(tx)),
            pool,
            writer_thread: Mutex::new(Some(writer_thread)),
            flusher_thread: Mutex::new(flusher_thread),
            flusher_shutdown,
            _keepalive: keepalive,
            queue_wait_ns: queue_wait,
            sql_exec_ns: sql_exec,
            flush_latency_ns: flush_latency,
        }))
    }

    /// Close writer + flusher threads. Called by `Drop` automatically.
    pub fn close(&self) {
        if let Some(tx) = self.writer_tx.lock().unwrap().take() {
            drop(tx);
        }
        if let Some(h) = self.writer_thread.lock().unwrap().take() {
            let _ = h.join();
        }
        self.flusher_shutdown.store(true, Ordering::Relaxed);
        if let Some(h) = self.flusher_thread.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

impl Drop for SqliteStore {
    fn drop(&mut self) {
        self.close();
    }
}

#[async_trait]
impl StanzaStore for SqliteStore {
    async fn init(&self) -> Result<(), StoreError> {
        let conn = self.pool.get().map_err(StoreError::backend)?;
        conn.execute_batch(MAM_SCHEMA)
            .map_err(StoreError::backend)?;
        Ok(())
    }

    async fn store_message(&self, m: &ArchivedMessage) -> Result<(), StoreError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let job = WriteJob {
            msg: m.clone(),
            enqueued_at: Instant::now(),
            reply: reply_tx,
        };
        let tx = {
            let guard = self.writer_tx.lock().unwrap();
            guard
                .as_ref()
                .ok_or_else(|| StoreError::Backend("writer closed".into()))?
                .clone()
        };
        match tx.try_send(job) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(job)) => {
                tokio::task::spawn_blocking(move || tx.send(job))
                    .await
                    .map_err(StoreError::backend)?
                    .map_err(|_| StoreError::Backend("writer disconnected".into()))?;
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err(StoreError::Backend("writer disconnected".into()));
            }
        }
        reply_rx
            .await
            .map_err(|_| StoreError::Backend("writer dropped reply".into()))?
    }

    async fn query_messages(
        &self,
        q: &bench_core::message::MamQuery,
    ) -> Result<Vec<ArchivedMessage>, StoreError> {
        let pool = self.pool.clone();
        let q = q.clone();
        tokio::task::spawn_blocking(move || run_query(&pool, &q))
            .await
            .map_err(StoreError::backend)?
    }

    async fn count_messages(&self, room_jid: &str) -> Result<u64, StoreError> {
        let pool = self.pool.clone();
        let room = room_jid.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(StoreError::backend)?;
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ?1",
                    [&room],
                    |r| r.get(0),
                )
                .map_err(StoreError::backend)?;
            Ok(n as u64)
        })
        .await
        .map_err(StoreError::backend)?
    }

    async fn db_size_bytes(&self) -> Result<u64, StoreError> {
        // Report the on-disk file size in both modes: for Disk that's the
        // live DB, for Memory that's the last snapshot. 0 if missing.
        let path = match &self.backing {
            SqliteBacking::Disk(p) => p.clone(),
            SqliteBacking::Memory { snapshot_to, .. } => snapshot_to.clone(),
        };
        tokio::task::spawn_blocking(move || match std::fs::metadata(&path) {
            Ok(m) => Ok(m.len()),
            Err(_) => Ok(0),
        })
        .await
        .map_err(StoreError::backend)?
    }

    async fn diagnostics(&self) -> Vec<OpStats> {
        let qw = self.queue_wait_ns.lock().unwrap();
        let se = self.sql_exec_ns.lock().unwrap();
        let fl = self.flush_latency_ns.lock().unwrap();
        let mut out = Vec::with_capacity(3);
        if let Some(s) = op_stats_from_hist("write_queue_wait", &qw) {
            out.push(s);
        }
        if let Some(s) = op_stats_from_hist("write_sql_exec", &se) {
            out.push(s);
        }
        if let Some(s) = op_stats_from_hist("flush", &fl) {
            out.push(s);
        }
        out
    }
}
