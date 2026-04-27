//! SQLite backend for the stanza-database benchmark.
//!
//! Two backing modes:
//!
//! * [`SqliteBacking::Disk`] — file on disk, WAL mode. This is the mode that
//!   matches waddle's current production setup.
//! * [`SqliteBacking::Memory`] — shared-cache in-memory DB. A flusher thread
//!   uses SQLite's online-backup API to snapshot pages to a disk file every
//!   `flush_interval`. Trades durability (up to `flush_interval` of write
//!   loss on crash) for near-RAM write speed.
//!
//! Both modes use:
//! * **One dedicated writer thread** fed via `std::sync::mpsc` — avoids
//!   `SQLITE_BUSY` thrash under high write contention. Disk-mode uses
//!   `synchronous=NORMAL`; memory-mode has no fsync cost on the hot path.
//! * **r2d2 reader pool** for the hot read path. WAL gives concurrent readers
//!   on disk; shared-cache URI gives concurrent readers on memory.
//!
//! Schema matches
//! `waddle/server/crates/waddle-xmpp/src/mam/storage.rs:226-271` byte-for-byte.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bench_core::message::{ArchivedMessage, MamQuery};
use bench_core::metrics::{op_stats_from_hist, OpStats};
use bench_core::store::{StanzaStore, StoreError};
use hdrhistogram::Histogram;
use r2d2::Pool;
use rusqlite::{params, Connection, OpenFlags};

/// SQL schema — copied from waddle's `MAM_SCHEMA` to stay byte-compatible.
pub const MAM_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mam_messages (
    id TEXT PRIMARY KEY,
    room_jid TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    from_jid TEXT NOT NULL,
    to_jid TEXT NOT NULL,
    body TEXT,
    message_id TEXT,
    thread_id TEXT,
    origin_id TEXT,
    message_type TEXT NOT NULL DEFAULT 'chat',
    stanza_xml TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_mam_room_timestamp
    ON mam_messages(room_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_sender
    ON mam_messages(room_jid, from_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_id
    ON mam_messages(room_jid, id);
"#;

/// How the DB pages are stored.
#[derive(Debug, Clone)]
pub enum SqliteBacking {
    /// On-disk WAL file. The path to the `.db`.
    Disk(PathBuf),
    /// Shared-cache in-memory DB, snapshotted to `snapshot_to` every
    /// `flush_interval`. `name` scopes the shared in-memory DB within the
    /// process — two `Memory` stores with different names are independent.
    Memory {
        name: String,
        snapshot_to: PathBuf,
        flush_interval: Duration,
    },
}

impl SqliteBacking {
    fn connection_uri(&self) -> String {
        match self {
            Self::Disk(p) => p.to_string_lossy().into_owned(),
            Self::Memory { name, .. } => {
                format!("file:{name}?mode=memory&cache=shared")
            }
        }
    }

    fn open_flags(&self) -> OpenFlags {
        let mut f = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
        // NO_MUTEX is fine here: rusqlite wraps Connection in its own Mutex
        // for Send bounds. For shared-cache memory we need URI parsing.
        if matches!(self, Self::Memory { .. }) {
            f |= OpenFlags::SQLITE_OPEN_URI;
        }
        f
    }

    fn on_disk(&self) -> bool {
        matches!(self, Self::Disk(_))
    }
}

fn apply_pragmas(conn: &Connection, backing: &SqliteBacking) -> rusqlite::Result<()> {
    // Pragmas differ between backings: WAL is only meaningful on disk, and
    // memory DBs have no fsync cost so `synchronous` is moot. Keep the rest
    // the same (temp_store, cache, busy_timeout).
    if backing.on_disk() {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "wal_autocheckpoint", 1000)?;
    } else {
        // In-memory: skip journal_mode (SQLite will pick MEMORY automatically
        // for :memory: DBs and reject WAL anyway).
        conn.pragma_update(None, "synchronous", "OFF")?;
        // CRITICAL for shared-cache mode: without this, any reader holds a
        // table-level read lock that makes the writer see SQLITE_LOCKED on
        // INSERT. busy_timeout does NOT apply to SQLITE_LOCKED (only BUSY),
        // so writes fail outright. read_uncommitted=1 lets readers proceed
        // without blocking the writer. Cost: readers may see uncommitted
        // rows — fine for a benchmark; waddle production is disk+WAL anyway.
        conn.pragma_update(None, "read_uncommitted", 1i64)?;
    }
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "mmap_size", 268_435_456i64)?;
    conn.pragma_update(None, "cache_size", -64_000i64)?; // 64 MiB
    conn.pragma_update(None, "busy_timeout", 5_000i64)?;
    Ok(())
}

/// r2d2 connection manager that opens connections via URI + explicit flags.
/// r2d2_sqlite's built-in manager uses `Connection::open` which doesn't let
/// us pass `SQLITE_OPEN_URI`, so we roll a minimal one.
struct UriManager {
    uri: String,
    flags: OpenFlags,
    backing: SqliteBacking,
}

impl r2d2::ManageConnection for UriManager {
    type Connection = Connection;
    type Error = rusqlite::Error;

    fn connect(&self) -> Result<Connection, rusqlite::Error> {
        let c = Connection::open_with_flags(&self.uri, self.flags)?;
        apply_pragmas(&c, &self.backing)?;
        Ok(c)
    }

    fn is_valid(&self, c: &mut Connection) -> Result<(), rusqlite::Error> {
        c.execute_batch("SELECT 1")
    }

    fn has_broken(&self, _c: &mut Connection) -> bool {
        false
    }
}

/// Message sent to the writer thread.
struct WriteJob {
    msg: ArchivedMessage,
    /// Instant at which the caller pushed this job onto the queue.
    enqueued_at: Instant,
    reply: tokio::sync::oneshot::Sender<Result<(), StoreError>>,
}

pub struct SqliteStore {
    backing: SqliteBacking,
    writer_tx: Mutex<Option<mpsc::SyncSender<WriteJob>>>,
    pool: Pool<UriManager>,
    writer_thread: Mutex<Option<thread::JoinHandle<()>>>,
    /// Only populated in `Memory` mode.
    flusher_thread: Mutex<Option<thread::JoinHandle<()>>>,
    flusher_shutdown: Arc<AtomicBool>,
    /// Number of successful flushes since open. Exposed via diagnostics.
    flush_count: Arc<std::sync::atomic::AtomicU64>,
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
        let flush_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
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
            flush_count,
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

fn writer_loop(
    uri: String,
    flags: OpenFlags,
    backing: SqliteBacking,
    rx: mpsc::Receiver<WriteJob>,
    queue_wait: Arc<Mutex<Histogram<u64>>>,
    sql_exec: Arc<Mutex<Histogram<u64>>>,
) {
    let conn = match Connection::open_with_flags(&uri, flags) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "writer failed to open db");
            return;
        }
    };
    if let Err(e) = apply_pragmas(&conn, &backing) {
        tracing::error!(error = %e, "writer failed to apply pragmas");
        return;
    }
    // Writer is authoritative for DDL. `CREATE IF NOT EXISTS` is idempotent
    // so running it again when the keepalive also created it is harmless.
    if let Err(e) = conn.execute_batch(MAM_SCHEMA) {
        tracing::error!(error = %e, "writer failed to init schema");
        return;
    }

    const INSERT_SQL: &str = r#"INSERT INTO mam_messages
            (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id,
             thread_id, origin_id, message_type, stanza_xml)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#;

    while let Ok(job) = rx.recv() {
        let qw = job.enqueued_at.elapsed();
        if let Ok(mut h) = queue_wait.lock() {
            let _ = h.record(qw.as_nanos().min(u64::MAX as u128) as u64);
        }

        let m = &job.msg;
        let ts = m.timestamp.to_rfc3339();
        let exec_start = Instant::now();
        let result = conn
            .prepare_cached(INSERT_SQL)
            .and_then(|mut stmt| {
                stmt.execute(params![
                    m.id,
                    m.room_jid,
                    ts,
                    m.from,
                    m.to,
                    m.body,
                    m.message_id,
                    m.thread_id,
                    m.origin_id,
                    m.message_type,
                    m.stanza_xml,
                ])
                .map(|_| ())
            })
            .map_err(StoreError::backend);
        let exec = exec_start.elapsed();
        if let Ok(mut h) = sql_exec.lock() {
            let _ = h.record(exec.as_nanos().min(u64::MAX as u128) as u64);
        }
        let _ = job.reply.send(result);
    }
}

/// Periodically snapshots the in-memory DB to a disk file via SQLite's
/// online backup API.
#[allow(clippy::too_many_arguments)]
fn flusher_loop(
    src_uri: String,
    src_flags: OpenFlags,
    src_backing: SqliteBacking,
    dst_path: PathBuf,
    interval: Duration,
    shutdown: Arc<AtomicBool>,
    flush_latency: Arc<Mutex<Histogram<u64>>>,
    flush_count: Arc<std::sync::atomic::AtomicU64>,
) {
    // One source connection to the shared-cache in-mem DB, reused across flushes.
    let src = match Connection::open_with_flags(&src_uri, src_flags) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "flusher failed to open source");
            return;
        }
    };
    if let Err(e) = apply_pragmas(&src, &src_backing) {
        tracing::error!(error = %e, "flusher failed to apply pragmas");
        return;
    }

    // Sleep in small steps so we notice shutdown quickly.
    let step = Duration::from_millis(250);
    let mut elapsed = Duration::ZERO;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        thread::sleep(step);
        elapsed += step;
        if elapsed < interval {
            continue;
        }
        elapsed = Duration::ZERO;
        if let Err(e) = do_flush(&src, &dst_path, &flush_latency, &flush_count) {
            tracing::warn!(error = %e, "flush failed");
        }
    }
    // Final flush on shutdown so disk has the latest state.
    if let Err(e) = do_flush(&src, &dst_path, &flush_latency, &flush_count) {
        tracing::warn!(error = %e, "final flush failed");
    }
}

fn do_flush(
    src: &Connection,
    dst_path: &Path,
    flush_latency: &Mutex<Histogram<u64>>,
    flush_count: &std::sync::atomic::AtomicU64,
) -> rusqlite::Result<()> {
    let t0 = Instant::now();
    // Fresh destination connection per flush — cheap, and closing at the end
    // flushes the OS page cache cleanly.
    let mut dst = Connection::open(dst_path)?;
    {
        let bk = rusqlite::backup::Backup::new(src, &mut dst)?;
        // Copy up to 1024 pages at a time with 5ms pauses between batches —
        // keeps writer throughput smooth while the backup runs.
        bk.run_to_completion(1024, Duration::from_millis(5), None)?;
    }
    // Drop forces close-and-sync.
    drop(dst);
    let dt = t0.elapsed();
    if let Ok(mut h) = flush_latency.lock() {
        let _ = h.record(dt.as_nanos().min(u64::MAX as u128) as u64);
    }
    flush_count.fetch_add(1, Ordering::Relaxed);
    Ok(())
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

    async fn query_messages(&self, q: &MamQuery) -> Result<Vec<ArchivedMessage>, StoreError> {
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

fn run_query(pool: &Pool<UriManager>, q: &MamQuery) -> Result<Vec<ArchivedMessage>, StoreError> {
    let conn = pool.get().map_err(StoreError::backend)?;
    let mut sql = String::from(
        "SELECT id, room_jid, timestamp, from_jid, to_jid, body, message_id, thread_id, \
         origin_id, message_type, stanza_xml \
         FROM mam_messages WHERE room_jid = ?1",
    );
    let mut params_dyn: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(q.room_jid.clone())];
    let mut idx: usize = 2;
    if let Some(start) = q.start {
        sql.push_str(&format!(" AND timestamp >= ?{idx}"));
        params_dyn.push(Box::new(start.to_rfc3339()));
        idx += 1;
    }
    if let Some(end) = q.end {
        sql.push_str(&format!(" AND timestamp <= ?{idx}"));
        params_dyn.push(Box::new(end.to_rfc3339()));
        idx += 1;
    }
    if let Some(from) = &q.from_jid {
        sql.push_str(&format!(" AND from_jid = ?{idx}"));
        params_dyn.push(Box::new(from.clone()));
        idx += 1;
    }
    if let Some(before) = &q.before_id {
        sql.push_str(&format!(" AND id < ?{idx}"));
        params_dyn.push(Box::new(before.clone()));
        idx += 1;
    }
    if let Some(after) = &q.after_id {
        sql.push_str(&format!(" AND id > ?{idx}"));
        params_dyn.push(Box::new(after.clone()));
        idx += 1;
    }
    sql.push_str(" ORDER BY timestamp DESC");
    let limit = q.limit.max(1);
    sql.push_str(&format!(" LIMIT ?{idx}"));
    params_dyn.push(Box::new(limit as i64));

    let refs: Vec<&dyn rusqlite::ToSql> = params_dyn.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(StoreError::backend)?;
    let rows = stmt
        .query_map(refs.as_slice(), row_to_message)
        .map_err(StoreError::backend)?;
    let mut out = Vec::with_capacity(limit as usize);
    for r in rows {
        out.push(r.map_err(StoreError::backend)?);
    }
    Ok(out)
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArchivedMessage> {
    let ts: String = row.get(2)?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(&ts)
        .map(|d| d.with_timezone(&chrono::Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?;
    Ok(ArchivedMessage {
        id: row.get(0)?,
        room_jid: row.get(1)?,
        timestamp,
        from: row.get(3)?,
        to: row.get(4)?,
        body: row.get(5)?,
        message_id: row.get(6)?,
        thread_id: row.get(7)?,
        origin_id: row.get(8)?,
        message_type: row.get(9)?,
        stanza_xml: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bench_core::message::ArchivedMessage;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn insert_and_query_roundtrip_disk() {
        let dir = tempdir();
        let path = dir.join("bench.db");
        let store = SqliteStore::open(&path, 4).unwrap();
        store.init().await.unwrap();
        populate_and_check(&store).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn insert_and_query_roundtrip_memory() {
        let dir = tempdir();
        let snapshot = dir.join("snapshot.db");
        let store = SqliteStore::open_with_backing(
            SqliteBacking::Memory {
                name: format!("bench-test-{}", uuid_like()),
                snapshot_to: snapshot.clone(),
                flush_interval: Duration::from_millis(200),
            },
            4,
        )
        .unwrap();
        store.init().await.unwrap();
        populate_and_check(&store).await;

        // Let at least one flush cycle run, then check the snapshot exists
        // and contains data.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let bytes = std::fs::metadata(&snapshot).unwrap().len();
        assert!(bytes > 0, "snapshot file should be non-empty");

        // Independent connection reading the snapshot should see the rows.
        let snap = Connection::open(&snapshot).unwrap();
        let n: i64 = snap
            .query_row(
                "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ?1",
                ["room1@conference.bench.local"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1_000);
    }

    async fn populate_and_check(store: &Arc<SqliteStore>) {
        for i in 0..1_000 {
            let mut m = ArchivedMessage::new_chat(
                "room1@conference.bench.local",
                &format!("user{i}@bench.local/c"),
                "room1@conference.bench.local",
                &format!("body {i}"),
            );
            m.message_type = "groupchat".into();
            store.store_message(&m).await.unwrap();
        }
        let count = store
            .count_messages("room1@conference.bench.local")
            .await
            .unwrap();
        assert_eq!(count, 1_000);
        let q = MamQuery {
            room_jid: "room1@conference.bench.local".into(),
            limit: 50,
            ..Default::default()
        };
        let rows = store.query_messages(&q).await.unwrap();
        assert_eq!(rows.len(), 50);
    }

    fn tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("bench-sqlite-test-{}", uuid_like()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{n}")
    }
}
