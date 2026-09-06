use std::time::{Duration, Instant};

use super::{IngressUnitOfWork, IngressUowError};
use crate::{
    config::LineageConfig,
    db::{Database, DatabaseConfig, DatabaseDriver},
};

#[tokio::test]
async fn sqlite_ingress_begin_with_timeouts_bounds_held_writer() {
    let directory = tempfile::tempdir().expect("temporary SQLite directory");
    let path = directory.path().join("ingress-lock.db");
    let config = DatabaseConfig {
        pool_size: 2,
        ..DatabaseConfig::new(DatabaseDriver::Sqlite, path.to_string_lossy().into_owned())
    };
    let db = Database::from_config("ingress-lock-timeout", &config)
        .await
        .expect("open file-backed SQLite database");
    let uow = IngressUnitOfWork::open(db.clone(), LineageConfig::default())
        .expect("open ingress unit of work");
    let writer = db.begin_immediate().await.expect("hold SQLite writer");
    let lock = Duration::from_millis(100);
    let started = Instant::now();
    // Keep the writer held throughout acquisition. The outer deadline makes
    // a regression fail promptly instead of waiting for SQLite's 5s timeout.
    let result = tokio::time::timeout(
        Duration::from_millis(250),
        uow.begin_with_timeouts(lock, Duration::from_millis(250)),
    )
    .await;
    let elapsed = started.elapsed();
    writer.commit().await.expect("release held writer");
    assert!(matches!(result, Ok(Err(IngressUowError::Timeout))));
    assert!(elapsed >= lock);
    assert!(elapsed < Duration::from_millis(250));

    // Cancellation must also leave the pooled connection usable once the
    // outstanding SQLite operation observes the released writer lock.
    let transaction = tokio::time::timeout(Duration::from_secs(1), db.begin_immediate())
        .await
        .expect("subsequent acquisition is not blocked")
        .expect("subsequent SQLite transaction");
    transaction.commit().await.expect("commit after timeout");
}
