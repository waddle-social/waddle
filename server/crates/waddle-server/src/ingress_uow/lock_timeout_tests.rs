use std::time::{Duration, Instant};

use super::{IngressUnitOfWork, IngressUowError};
use crate::{
    config::LineageConfig,
    db::{Database, DatabaseConfig, DatabaseDriver},
};

#[tokio::test(start_paused = true)]
async fn ingress_transaction_acquisition_expiry_maps_to_timeout_without_database() {
    let lock = Duration::from_millis(100);
    let started = tokio::time::Instant::now();
    let result = super::acquire_transaction_with_timeout(lock, std::future::pending()).await;
    assert!(matches!(result, Err(IngressUowError::Timeout)));
    assert_eq!(started.elapsed(), lock);
}

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

async fn assert_ingress_checkout_timeout(db: Database) {
    let uow = IngressUnitOfWork::open(db.clone(), LineageConfig::default())
        .expect("open ingress unit of work");
    let held = db.begin().await.expect("saturate single-connection pool");
    let lock = Duration::from_millis(100);
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_millis(250),
        uow.begin_with_timeouts(lock, Duration::from_millis(250)),
    )
    .await;
    let elapsed = started.elapsed();
    assert!(matches!(result, Ok(Err(IngressUowError::Timeout))));
    assert!(elapsed >= lock);
    assert!(elapsed < Duration::from_millis(250));
    held.commit().await.expect("release pooled connection");
    let transaction = tokio::time::timeout(Duration::from_secs(1), db.begin())
        .await
        .expect("cancelled checkout leaves pool usable")
        .expect("begin after checkout timeout");
    transaction.commit().await.expect("commit after timeout");
}

#[tokio::test]
async fn sqlite_ingress_begin_with_timeouts_maps_pool_checkout_expiry_to_timeout() {
    let directory = tempfile::tempdir().expect("temporary SQLite directory");
    let config = DatabaseConfig {
        pool_size: 1,
        ..DatabaseConfig::new(
            DatabaseDriver::Sqlite,
            directory
                .path()
                .join("checkout.db")
                .to_string_lossy()
                .into_owned(),
        )
    };
    let db = Database::from_config("ingress-checkout-timeout", &config)
        .await
        .expect("open SQLite database");
    assert_ingress_checkout_timeout(db).await;
}

#[tokio::test]
async fn postgres_ingress_begin_with_timeouts_bounds_pool_checkout() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping postgres_ingress_begin_with_timeouts_bounds_pool_checkout: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let config = DatabaseConfig {
        pool_size: 1,
        ..DatabaseConfig::new(DatabaseDriver::Postgres, database_url)
    };
    let db = Database::from_config("ingress-checkout-timeout", &config)
        .await
        .expect("open PostgreSQL database");
    assert_ingress_checkout_timeout(db).await;
}
