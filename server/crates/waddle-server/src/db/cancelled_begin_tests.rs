//! A cancelled `BEGIN IMMEDIATE` must not strand SQLite's write lock on an
//! idle pooled connection.
use super::Database;
use std::time::Duration;

/// Sweeps the caller's timeout across the whole acquisition so one of them
/// lands between sqlx's `BEGIN IMMEDIATE` and the `Transaction` it guards. A
/// stranded lock makes the observer wait out `busy_timeout` and fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timed_out_begin_immediate_releases_the_write_lock() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("timed-out-begin.db");
    let cancelled = Database::open_local("cancelled", &path)
        .await
        .expect("cancelled handle");
    let observer = Database::open_local("observer", &path)
        .await
        .expect("observer handle");
    for micros in (0..1500).step_by(10) {
        let _ =
            tokio::time::timeout(Duration::from_micros(micros), cancelled.begin_immediate()).await;
        // An abandoned acquisition finishes and rolls back on its own task.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let started = std::time::Instant::now();
        let transaction = observer.begin_immediate().await;
        assert!(
            transaction.is_ok(),
            "write lock stranded after a {micros}us timeout: {:?} after {:?}",
            transaction.err(),
            started.elapsed()
        );
    }
}
