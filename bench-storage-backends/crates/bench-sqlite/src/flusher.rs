use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use rusqlite::{Connection, OpenFlags};

use crate::schema::apply_pragmas;
use crate::SqliteBacking;

/// Periodically snapshots the in-memory DB to a disk file via SQLite's
/// online backup API.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flusher_loop(
    src_uri: String,
    src_flags: OpenFlags,
    src_backing: SqliteBacking,
    dst_path: PathBuf,
    interval: Duration,
    shutdown: Arc<AtomicBool>,
    flush_latency: Arc<Mutex<Histogram<u64>>>,
    flush_count: Arc<AtomicU64>,
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
    flush_count: &AtomicU64,
) -> rusqlite::Result<()> {
    let t0 = Instant::now();
    // Fresh destination connection per flush - cheap, and closing at the end
    // flushes the OS page cache cleanly.
    let mut dst = Connection::open(dst_path)?;
    {
        let bk = rusqlite::backup::Backup::new(src, &mut dst)?;
        // Copy up to 1024 pages at a time with 5ms pauses between batches -
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
