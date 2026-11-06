//! PG17 witnesses for the dark cooperative catalog mutation fence.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};
use waddle_server::db::physical_attestation::{
    CatalogFenceExclusive, CatalogFenceIndex, CatalogFenceShared, CatalogMutationFenceError,
};

use super::test_url;

const WAIT: Duration = Duration::from_secs(15);

fn key() -> i64 {
    let id = uuid::Uuid::new_v4();
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&id.as_bytes()[..8]);
    let key = i64::from_be_bytes(bytes);
    if key == 0 {
        1
    } else {
        key
    }
}

async fn wait_for_idle_try(pool: &PgPool, application: &str, query_prefix: &str) -> i32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = $1 \
                 AND state = 'idle' AND xact_start IS NULL \
                 AND pg_catalog.starts_with(query, $2)",
            )
            .bind(application)
            .bind(query_prefix)
            .fetch_optional(pool)
            .await
            .expect("inspect idle try-lock session");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("try-lock session completed an attempt without an open transaction")
}

async fn wait_for_backend_lock(pool: &PgPool, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE pid = $1 AND state = 'active' AND wait_event_type = 'Lock')",
            )
            .bind(pid)
            .fetch_one(pool)
            .await
            .expect("inspect maintenance backend");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("concurrent reindex waited for the existing writer");
}

#[tokio::test]
async fn catalog_mutation_fence_blocks_and_survives_concurrent_reindex() {
    let Some(url) = test_url() else { return };
    let options = PgConnectOptions::from_str(&url).expect("PostgreSQL options");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options.clone())
        .await
        .expect("observer pool");
    let schema = format!("fence_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&pool)
        .await
        .expect("disposable schema");
    sqlx::query(&format!("CREATE TABLE {schema}.items (id bigint NOT NULL)"))
        .execute(&pool)
        .await
        .expect("disposable table");
    sqlx::query(&format!("CREATE INDEX items_idx ON {schema}.items (id)"))
        .execute(&pool)
        .await
        .expect("disposable index");
    let index = CatalogFenceIndex::new(&schema, "items_idx").expect("fixed index");
    let key = key();

    let mut reader_a = CatalogFenceShared::acquire(&options, key, WAIT)
        .await
        .expect("first shared session fence");
    let reader_b = CatalogFenceShared::acquire(&options, key, WAIT)
        .await
        .expect("second shared session fence");
    // Advisory locking does not constrain a privileged mutator that ignores
    // the protocol. A trusted external policy is therefore indispensable.
    sqlx::query(&format!("REINDEX INDEX CONCURRENTLY {schema}.items_idx"))
        .execute(&pool)
        .await
        .expect("uncooperative maintenance bypasses advisory guards");
    reader_a
        .verify()
        .await
        .expect("reader remains locked despite bypass");
    let mut transaction_reader = PgConnection::connect_with(&options)
        .await
        .expect("independent transaction reader");
    sqlx::query("BEGIN")
        .execute(&mut transaction_reader)
        .await
        .expect("begin transaction reader");
    let acquired: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
            .bind(key)
            .fetch_one(&mut transaction_reader)
            .await
            .expect("transaction-scoped shared fence");
    assert!(acquired);
    let maintainer_name = format!("p21_maint_{}", uuid::Uuid::new_v4().simple());
    let maintainer_options = options.clone().application_name(&maintainer_name);
    let mut maintainer = tokio::spawn(async move {
        CatalogFenceExclusive::acquire(&maintainer_options, key, WAIT).await
    });
    wait_for_idle_try(
        &pool,
        &maintainer_name,
        "SELECT pg_catalog.pg_try_advisory_lock(",
    )
    .await;
    reader_a.release().await.expect("release first reader");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut maintainer)
            .await
            .is_err(),
        "second reader still fences maintenance"
    );
    reader_b.release().await.expect("release second reader");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut maintainer)
            .await
            .is_err(),
        "transaction-scoped reader still fences maintenance"
    );
    sqlx::query("COMMIT")
        .execute(&mut transaction_reader)
        .await
        .expect("release transaction-scoped reader");
    let mut maintainer = maintainer
        .await
        .expect("maintenance task")
        .expect("exclusive session fence");
    let maintainer_pid = maintainer.backend_pid();

    // Hold a write transaction so PG17's concurrent reindex reaches a real
    // wait between its internal transactions. The advisory lock must remain
    // held on that same maintenance backend throughout the wait and command.
    let mut writer = PgConnection::connect_with(&options)
        .await
        .expect("independent writer");
    sqlx::query("BEGIN")
        .execute(&mut writer)
        .await
        .expect("begin writer");
    sqlx::query(&format!("INSERT INTO {schema}.items VALUES (1)"))
        .execute(&mut writer)
        .await
        .expect("hold writer transaction");
    let reindex = tokio::spawn(async move {
        let result = maintainer.reindex(&index).await;
        (maintainer, result)
    });
    wait_for_backend_lock(&pool, maintainer_pid).await;
    let reader_name = format!("p21_reader_{}", uuid::Uuid::new_v4().simple());
    let reader_options = options.clone().application_name(&reader_name);
    let mut reader =
        tokio::spawn(async move { CatalogFenceShared::acquire(&reader_options, key, WAIT).await });
    let reader_pid = wait_for_idle_try(
        &pool,
        &reader_name,
        "SELECT pg_catalog.pg_try_advisory_lock_shared(",
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut reader)
            .await
            .is_err()
    );
    sqlx::query("COMMIT")
        .execute(&mut writer)
        .await
        .expect("release writer");
    let (mut maintainer, result) = reindex.await.expect("reindex task");
    result.expect("REINDEX INDEX CONCURRENTLY completed on fenced backend");
    maintainer
        .verify()
        .await
        .expect("exclusive lock survived reindex");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut reader)
            .await
            .is_err(),
        "same reader remains fenced after reindex"
    );
    maintainer.release().await.expect("release maintainer");
    let reader = reader
        .await
        .expect("reader task")
        .expect("same reader admitted after exclusive release");
    assert_eq!(reader.backend_pid(), reader_pid);
    reader.release().await.expect("release reader");
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .expect("drop disposable schema");
}

#[tokio::test]
async fn catalog_mutation_fence_refuses_contention_and_lost_sessions() {
    let Some(url) = test_url() else { return };
    let options = PgConnectOptions::from_str(&url).expect("PostgreSQL options");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone())
        .await
        .expect("observer pool");
    let key = key();
    assert!(matches!(
        CatalogFenceIndex::new("public; DROP SCHEMA public", "idx"),
        Err(CatalogMutationFenceError::InvalidIndex)
    ));
    assert!(matches!(
        CatalogFenceShared::acquire(&options, 0, WAIT).await,
        Err(CatalogMutationFenceError::InvalidBounds)
    ));
    let reader = CatalogFenceShared::acquire(&options, key, WAIT)
        .await
        .expect("shared fence");
    assert!(matches!(
        CatalogFenceExclusive::acquire(&options, key, Duration::from_millis(100)).await,
        Err(CatalogMutationFenceError::Contended)
    ));
    reader.release().await.expect("release reader");

    let mut maintainer = CatalogFenceExclusive::acquire(&options, key, WAIT)
        .await
        .expect("exclusive fence");
    let pid = maintainer.backend_pid();
    let terminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&pool)
        .await
        .expect("terminate maintenance backend");
    assert!(terminated);
    assert!(matches!(
        maintainer.verify().await,
        Err(CatalogMutationFenceError::FenceUnconfirmed { backend_pid }) if backend_pid == pid
    ));
    let index = CatalogFenceIndex::new("public", "unused_index").expect("valid identifier");
    assert!(matches!(
        maintainer.reindex(&index).await,
        Err(CatalogMutationFenceError::FenceLost)
    ));
    assert!(matches!(
        maintainer.release().await,
        Err(CatalogMutationFenceError::FenceLost)
    ));

    let mut maintainer = CatalogFenceExclusive::acquire(&options, key, WAIT)
        .await
        .expect("new exclusive fence after terminated backend");
    let absent_schema = format!("missing_{}", uuid::Uuid::new_v4().simple());
    let missing =
        CatalogFenceIndex::new(&absent_schema, "missing_idx").expect("valid but nonexistent index");
    assert!(matches!(
        maintainer.reindex(&missing).await,
        Err(CatalogMutationFenceError::MaintenanceUnconfirmed { .. })
    ));
    assert!(matches!(
        maintainer.verify().await,
        Err(CatalogMutationFenceError::FenceLost)
    ));
}
#[tokio::test]
async fn catalog_mutation_fence_cancelled_reindex_requires_backend_reconciliation() {
    let Some(url) = test_url() else { return };
    let options = PgConnectOptions::from_str(&url).expect("PostgreSQL options");
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options.clone())
        .await
        .expect("observer pool");
    let schema = format!("fence_cancel_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&pool)
        .await
        .expect("disposable schema");
    sqlx::query(&format!("CREATE TABLE {schema}.items (id bigint NOT NULL)"))
        .execute(&pool)
        .await
        .expect("disposable table");
    sqlx::query(&format!("CREATE INDEX items_idx ON {schema}.items (id)"))
        .execute(&pool)
        .await
        .expect("disposable index");
    let index = CatalogFenceIndex::new(&schema, "items_idx").expect("fixed index");
    let key = key();
    let mut maintainer = CatalogFenceExclusive::acquire(&options, key, WAIT)
        .await
        .expect("exclusive session fence");
    let pid = maintainer.backend_pid();
    let mut writer = PgConnection::connect_with(&options)
        .await
        .expect("independent writer");
    sqlx::query("BEGIN")
        .execute(&mut writer)
        .await
        .expect("begin writer");
    sqlx::query(&format!("INSERT INTO {schema}.items VALUES (1)"))
        .execute(&mut writer)
        .await
        .expect("hold writer transaction");
    let mut operation = Box::pin(maintainer.reindex(&index));
    tokio::select! {
        result = &mut operation => panic!("reindex finished before cancellation: {result:?}"),
        _ = wait_for_backend_lock(&pool, pid) => {}
    }
    drop(operation);
    assert!(matches!(
        maintainer.verify().await,
        Err(CatalogMutationFenceError::FenceLost)
    ));
    sqlx::query("COMMIT")
        .execute(&mut writer)
        .await
        .expect("release writer");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let gone: bool = sqlx::query_scalar(
                "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
            )
            .bind(pid)
            .fetch_one(&pool)
            .await
            .expect("inspect original backend");
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("original backend termination independently observed");
    let replacement = CatalogFenceExclusive::acquire(&options, key, WAIT)
        .await
        .expect("new fence after backend reconciliation");
    replacement.release().await.expect("release replacement");
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .expect("drop disposable schema");
}
