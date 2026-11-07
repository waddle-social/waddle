//! Cancellation must not return an uncertain catalog-owning session to a pool.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;

use super::Database;

#[tokio::test]
async fn cancelled_catalog_transaction_discards_its_checkout() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("one-slot PostgreSQL pool");
    let db = Database::from_postgres_pool("catalog cancellation", pool.clone());
    let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
    let mut operation = Box::pin(async {
        let mut tx = db
            .begin_catalog_fenced(&[])
            .await
            .expect("acquire catalog fence");
        let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(tx.postgres_connection().expect("PostgreSQL transaction"))
            .await
            .expect("original backend PID");
        pid_tx.send(pid).expect("report acquired backend");
        // Keep the actual acquired transaction live until its future is
        // cancelled, rather than explicitly committing or rolling it back.
        std::future::pending::<()>().await;
        drop(tx);
    });
    let original_pid = tokio::select! {
        () = &mut operation => panic!("catalog transaction unexpectedly completed"),
        result = tokio::time::timeout(Duration::from_secs(20), pid_rx) => {
            result.expect("catalog acquisition timed out").expect("backend PID sent")
        }
    };
    drop(operation);

    let mut next = tokio::time::timeout(Duration::from_secs(10), db.begin())
        .await
        .expect("pool makes another checkout")
        .expect("begin on replacement checkout");
    let replacement_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(next.postgres_connection().expect("PostgreSQL transaction"))
        .await
        .expect("replacement backend PID");
    assert_ne!(
        replacement_pid, original_pid,
        "an uncertain transaction's checkout must not be reused"
    );
    next.rollback()
        .await
        .expect("finish replacement transaction");
    drop(db);
    pool.close().await;

    // The replacement PID proves only that the checkout was discarded. A
    // closed socket does not prove when the original backend ended or released
    // its locks; this test makes no such claim.
}
