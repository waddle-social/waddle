//! Real PostgreSQL witnesses for the production migration runner's catalog fence.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, PgPool};
use waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, DatabaseError, MigrationRunner};

use super::{make_v1028_pending, Fixture};
use waddle_server::db::run_dark_postgres_owner;

const LEDGER_KEY: i64 = 6_841_445_497_037_937_992;

async fn lose_commit_reply(
    listener: tokio::net::TcpListener,
    target: PgConnectOptions,
    reply_held: Arc<tokio::sync::Notify>,
    disconnect: Arc<tokio::sync::Notify>,
) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (client, _) = listener.accept().await?;
    let server = super::postgres_proxy::connect_local_postgres(&target).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let hold = Arc::new(AtomicBool::new(false));
    let on_commit = hold.clone();
    let upstream = async move {
        let mut buffer = [0_u8; 8192];
        let mut tail = Vec::new();
        loop {
            let count = client_read.read(&mut buffer).await?;
            if count == 0 {
                return Ok::<(), std::io::Error>(());
            }
            let mut scan = tail;
            scan.extend_from_slice(&buffer[..count]);
            if scan
                .windows(b"COMMIT\0".len())
                .any(|part| part == b"COMMIT\0")
            {
                on_commit.store(true, Ordering::SeqCst);
            }
            server_write.write_all(&buffer[..count]).await?;
            tail = scan[scan.len().saturating_sub(b"COMMIT\0".len() - 1)..].to_vec();
        }
    };
    let downstream = async move {
        let mut buffer = [0_u8; 8192];
        loop {
            let count = server_read.read(&mut buffer).await?;
            if count == 0 {
                return Ok::<(), std::io::Error>(());
            }
            if hold.load(Ordering::SeqCst) {
                reply_held.notify_one();
                disconnect.notified().await;
                return Ok(());
            }
            client_write.write_all(&buffer[..count]).await?;
        }
    };
    tokio::select! { result = upstream => result, result = downstream => result }
}

async fn wait_for_idle_retry(admin: &PgPool, application: &str) -> i32 {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let pid = sqlx::query_scalar::<_, i32>(
                "SELECT a.pid FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.state = 'idle' AND a.query = 'ROLLBACK' \
                   AND a.xact_start IS NULL AND a.backend_xmin IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
                     WHERE l.pid = a.pid AND l.locktype = 'advisory')",
            )
            .bind(application)
            .fetch_optional(admin)
            .await
            .expect("inspect retrying migration session");
            if let Some(pid) = pid {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("failed try ends its transaction, snapshot, and advisory locks")
}

async fn wait_for_fenced_migration_waiter(admin: &PgPool, application: &str) -> i32 {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT a.pid FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.wait_event_type = 'Lock' \
                 AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
                   WHERE pid = a.pid AND locktype = 'advisory' AND granted \
                     AND mode = 'ExclusiveLock' AND objsubid = 1 \
                     AND ((classid::bigint << 32) | objid::bigint) = $2) \
                 AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
                   WHERE pid = a.pid AND locktype = 'advisory' AND granted \
                     AND mode = 'ExclusiveLock' AND objsubid = 1 \
                     AND ((classid::bigint << 32) | objid::bigint) = $3)",
            )
            .bind(application)
            .bind(LEDGER_KEY)
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_optional(admin)
            .await
            .expect("inspect migration lock order");
            if let Some(pid) = pid {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("migration waiting on relation holds catalog and ledger locks")
}

async fn shared_try(connection: &mut PgConnection) -> bool {
    sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock_shared($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(connection)
        .await
        .expect("try shared catalog fence")
}

async fn unlock_shared(connection: &mut PgConnection) {
    let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock_shared($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(connection)
        .await
        .expect("release shared catalog fence");
    assert!(unlocked);
}

async fn wait_for_shared(connection: &mut PgConnection) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !shared_try(connection).await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("original backend eventually released the exclusive fence");
}

#[tokio::test]
async fn postgres_dark_owner_waits_for_catalog_before_ledger_or_ddl() {
    let Some(cluster_url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&cluster_url, true).await;
    fixture.enable_owner_handoff().await;
    make_v1028_pending(&fixture).await;

    let mut blocker = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("connect catalog reader");
    sqlx::query("BEGIN")
        .execute(&mut blocker)
        .await
        .expect("begin catalog reader");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock_shared($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut blocker)
        .await
        .expect("hold catalog reader");

    let application = format!("dark_owner_fence_{}", uuid::Uuid::new_v4().simple());
    let options = fixture
        .migrator_options
        .clone()
        .application_name(&application);
    let config = fixture.config();
    let migrator = fixture.migrator_role();
    let mut owner =
        tokio::spawn(async move { run_dark_postgres_owner(&options, &config, &migrator).await });

    wait_for_idle_retry(&fixture.admin, &application).await;
    assert!(tokio::time::timeout(Duration::from_millis(100), &mut owner)
        .await
        .is_err());
    let pending: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}._migrations WHERE version >= 1028",
        fixture.schema
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect pending owner catalog");
    assert_eq!(pending, 0, "catalog reader prevented migration DDL");

    sqlx::query("ROLLBACK")
        .execute(&mut blocker)
        .await
        .expect("release catalog reader");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(20), owner)
            .await
            .expect("owner resumes after catalog reader")
            .expect("owner task")
            .expect("owner commits pending catalog"),
        vec![1028, 1029, 1030]
    );
    blocker.close().await.expect("close catalog reader");
    fixture.cleanup().await;
}

#[tokio::test]
async fn sqlite_migration_runner_still_applies_once() {
    let db = Database::in_memory("sqlite migration witness")
        .await
        .expect("open SQLite database");
    let applied = MigrationRunner::single()
        .run(&db)
        .await
        .expect("apply SQLite migrations");
    assert!(!applied.is_empty());
    assert!(MigrationRunner::single()
        .run(&db)
        .await
        .expect("repeat SQLite migrations")
        .is_empty());
}

#[tokio::test]
async fn postgres_migration_catalog_fence_orders_retries_and_reindex() {
    let Some(cluster_url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    // PostgreSQL advisory locks are database-wide, so a schema alone cannot
    // isolate this contention witness from migrations in unrelated test suites.
    let cluster = PgPool::connect(&cluster_url)
        .await
        .expect("connect cluster observer");
    let database = format!("migration_fence_db_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&cluster)
        .await
        .expect("create isolated database");
    let mut database_url = url::Url::parse(&cluster_url).expect("PostgreSQL URL");
    database_url.set_path(&format!("/{database}"));
    let url = database_url.to_string();
    let admin = PgPool::connect(&url).await.expect("connect observer");
    let version: i32 = sqlx::query_scalar(
        "SELECT pg_catalog.current_setting('server_version_num')::pg_catalog.int4",
    )
    .fetch_one(&admin)
    .await
    .expect("server version");
    assert!((170_000..180_000).contains(&version), "PG17 is required");
    let schema = format!("migration_fence_{}", uuid::Uuid::new_v4().simple());
    let application = format!("migration_fence_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let mut scoped = url::Url::parse(&url).expect("PostgreSQL URL");
    scoped
        .query_pairs_mut()
        .append_pair("options", &format!("-c search_path={schema}"))
        .append_pair("application_name", &application);
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, scoped.to_string());
    config.pool_size = 1;
    let db = Database::from_config("migration fence witness", &config)
        .await
        .expect("open one-connection migration pool");

    let mut blocker = PgConnection::connect(&url).await.expect("blocker session");
    sqlx::query("BEGIN")
        .execute(&mut blocker)
        .await
        .expect("begin ledger blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(LEDGER_KEY)
        .execute(&mut blocker)
        .await
        .expect("hold ledger lock");
    assert!(shared_try(&mut blocker).await);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let migration_db = db.clone();
            let mut run = tokio::task::spawn_local(async move {
                MigrationRunner::single().run(&migration_db).await
            });
            wait_for_idle_retry(&admin, &application).await;
            assert!(tokio::time::timeout(Duration::from_millis(100), &mut run)
                .await
                .is_err());
            unlock_shared(&mut blocker).await;
            let mut observer = PgConnection::connect(&url).await.expect("observer session");
            wait_for_shared(&mut observer).await;
            unlock_shared(&mut observer).await;

            // The ledger-only blocker remains held through the full bounded
            // retry period. Each failed ledger try releases the catalog fence.
            let result = tokio::time::timeout(Duration::from_secs(20), &mut run)
                .await
                .expect("ledger contention is bounded")
                .expect("runner task");
            assert!(matches!(
                result,
                Err(DatabaseError::MigrationLedgerLockContended)
            ));
            let still_blocked: bool =
                sqlx::query_scalar("SELECT NOT pg_catalog.pg_try_advisory_xact_lock($1)")
                    .bind(LEDGER_KEY)
                    .fetch_one(&mut observer)
                    .await
                    .expect("verify ledger blocker remains held");
            assert!(still_blocked);
            assert!(shared_try(&mut observer).await);
            unlock_shared(&mut observer).await;
            let ledger: Option<String> =
                sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
                    .bind(format!("{schema}._migrations"))
                    .fetch_one(&admin)
                    .await
                    .expect("inspect refused migration");
            assert!(ledger.is_none(), "refused runner did not start migrations");
            sqlx::query("COMMIT")
                .execute(&mut blocker)
                .await
                .expect("release ledger blocker only after refusal");

            // Wait inside an actual schema operation with both advisory locks
            // held, then cancel. A cancellation cannot prove that the server
            // ended the in-flight statement or released the fence.
            sqlx::query(&format!(
                "CREATE TABLE {schema}._migrations (version bigint PRIMARY KEY, \
                 description text NOT NULL, checksum text, \
                 applied_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP)"
            ))
            .execute(&admin)
            .await
            .expect("create empty ledger");
            sqlx::query("BEGIN")
                .execute(&mut blocker)
                .await
                .expect("begin relation blocker");
            sqlx::query(&format!(
                "LOCK TABLE {schema}._migrations IN ACCESS EXCLUSIVE MODE"
            ))
            .execute(&mut blocker)
            .await
            .expect("hold relation lock");
            let migration_db = db.clone();
            let run = tokio::task::spawn_local(async move {
                MigrationRunner::single().run(&migration_db).await
            });
            wait_for_fenced_migration_waiter(&admin, &application).await;
            assert!(!shared_try(&mut observer).await);

            run.abort();
            assert!(run.await.expect_err("cancelled runner").is_cancelled());
            assert!(!shared_try(&mut observer).await);
            sqlx::query("COMMIT")
                .execute(&mut blocker)
                .await
                .expect("release relation blocker");
            wait_for_shared(&mut observer).await;
            unlock_shared(&mut observer).await;
            let rows: i64 =
                sqlx::query_scalar(&format!("SELECT count(*) FROM {schema}._migrations"))
                    .fetch_one(&admin)
                    .await
                    .expect("inspect cancelled migration ledger");
            assert_eq!(rows, 0, "cancelled runner recorded no migrations");

            let concurrent_db = Database::from_config("concurrent migration witness", &config)
                .await
                .expect("open independent migration pool");
            let first_runner = MigrationRunner::single();
            let second_runner = MigrationRunner::single();
            let (first, second) =
                tokio::join!(first_runner.run(&db), second_runner.run(&concurrent_db));
            let first = first.expect("first concurrent runner");
            let second = second.expect("second concurrent runner");
            let applied = if first.is_empty() {
                assert!(!second.is_empty());
                second
            } else {
                assert!(second.is_empty());
                first
            };
            drop(concurrent_db);
            assert!(MigrationRunner::single()
                .run(&db)
                .await
                .expect("repeat migration")
                .is_empty());
            let ledger_count: i64 =
                sqlx::query_scalar(&format!("SELECT count(*) FROM {schema}._migrations"))
                    .fetch_one(&admin)
                    .await
                    .expect("count committed ledger rows");
            assert_eq!(ledger_count, applied.len() as i64);

            // Run actual concurrent index maintenance on the same backend as
            // its session fence, while an older repeatable-read snapshot exists.
            sqlx::query(&format!("CREATE TABLE {schema}.fence_items (id bigint)"))
                .execute(&admin)
                .await
                .expect("maintenance table");
            sqlx::query(&format!(
                "CREATE INDEX fence_items_idx ON {schema}.fence_items (id)"
            ))
            .execute(&admin)
            .await
            .expect("maintenance index");
            let mut maintainer = PgConnection::connect(&url)
                .await
                .expect("maintenance session");
            sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
                .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                .execute(&mut maintainer)
                .await
                .expect("maintenance exclusive session fence");
            let maintainer_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
                .fetch_one(&mut maintainer)
                .await
                .expect("maintenance PID");
            let mut old = PgConnection::connect(&url)
                .await
                .expect("old snapshot session");
            sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
                .execute(&mut old)
                .await
                .expect("begin old snapshot");
            sqlx::query(&format!("SELECT count(*) FROM {schema}.fence_items"))
                .fetch_one(&mut old)
                .await
                .expect("establish old snapshot");
            let index = format!("{schema}.fence_items_idx");
            let reindex = tokio::spawn(async move {
                let result = sqlx::query(&format!("REINDEX INDEX CONCURRENTLY {index}"))
                    .execute(&mut maintainer)
                    .await;
                (maintainer, result)
            });
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    let waiting: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_progress_create_index \
                         WHERE pid = $1 AND phase = 'waiting for old snapshots')",
                    )
                    .bind(maintainer_pid)
                    .fetch_one(&admin)
                    .await
                    .expect("inspect reindex phase");
                    if waiting {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("reindex reaches the old snapshot wait");
            let migration_db = db.clone();
            let mut retry = tokio::task::spawn_local(async move {
                MigrationRunner::single().run(&migration_db).await
            });
            wait_for_idle_retry(&admin, &application).await;
            sqlx::query("ROLLBACK")
                .execute(&mut old)
                .await
                .expect("release old snapshot");
            let (mut maintainer, result) = tokio::time::timeout(Duration::from_secs(8), reindex)
                .await
                .expect("retrying runner does not hold an old snapshot")
                .expect("reindex task");
            result.expect("concurrent reindex completes");
            assert!(tokio::time::timeout(Duration::from_millis(100), &mut retry)
                .await
                .is_err());
            let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
                .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                .fetch_one(&mut maintainer)
                .await
                .expect("release maintenance fence");
            assert!(unlocked);
            assert!(retry
                .await
                .expect("retry task")
                .expect("retry migration")
                .is_empty());

            assert!(shared_try(&mut observer).await);
            let error = MigrationRunner::single()
                .run(&db)
                .await
                .expect_err("bounded contention must refuse migration");
            assert!(matches!(
                error,
                DatabaseError::MigrationCatalogFenceContended
            ));
            unlock_shared(&mut observer).await;

            // Lose the server's COMMIT reply after a fresh migration has
            // actually committed. The caller must see an error, and a fresh
            // runner must reconcile the committed ledger without replaying.
            let unknown_schema = format!("{schema}_unknown");
            sqlx::query(&format!("CREATE SCHEMA {unknown_schema}"))
                .execute(&admin)
                .await
                .expect("unknown commit schema");
            let mut direct_url = url::Url::parse(&url).expect("direct PostgreSQL URL");
            direct_url
                .query_pairs_mut()
                .append_pair("options", &format!("-c search_path={unknown_schema}"));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind local commit-reply proxy");
            let port = listener.local_addr().expect("proxy address").port();
            let reply_held = Arc::new(tokio::sync::Notify::new());
            let disconnect = Arc::new(tokio::sync::Notify::new());
            let proxy = tokio::spawn(lose_commit_reply(
                listener,
                PgConnectOptions::from_str(&url).expect("direct PostgreSQL options"),
                reply_held.clone(),
                disconnect.clone(),
            ));
            let proxy_url = super::postgres_proxy::local_proxy_url(direct_url.as_str(), port)
                .expect("loopback proxy URL");
            let mut proxy_config =
                DatabaseConfig::new(DatabaseDriver::Postgres, proxy_url.to_string());
            proxy_config.pool_size = 1;
            let proxy_db = Database::from_config("unknown migration commit", &proxy_config)
                .await
                .expect("open proxied migration connection");
            let pending_db = proxy_db.clone();
            let pending = tokio::task::spawn_local(async move {
                MigrationRunner::single().run(&pending_db).await
            });
            tokio::time::timeout(Duration::from_secs(10), reply_held.notified())
                .await
                .expect("server COMMIT reply held by proxy");
            let committed: i64 = sqlx::query_scalar(&format!(
                "SELECT count(*) FROM {unknown_schema}._migrations"
            ))
            .fetch_one(&admin)
            .await
            .expect("read committed ledger independently");
            assert_eq!(committed, applied.len() as i64);
            disconnect.notify_one();
            proxy.await.expect("proxy task").expect("proxy transport");
            assert!(pending.await.expect("migration task").is_err());
            drop(proxy_db);

            let direct_config =
                DatabaseConfig::new(DatabaseDriver::Postgres, direct_url.to_string());
            let direct_db = Database::from_config("reconcile migration commit", &direct_config)
                .await
                .expect("open fresh reconciliation connection");
            assert!(MigrationRunner::single()
                .run(&direct_db)
                .await
                .expect("reconcile committed migration")
                .is_empty());
            drop(direct_db);
            sqlx::query(&format!("DROP SCHEMA {unknown_schema} CASCADE"))
                .execute(&admin)
                .await
                .expect("drop unknown commit schema");
        })
        .await;
    drop(blocker);
    drop(db);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let gone: bool = sqlx::query_scalar(
                "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE datname = $1 AND application_name = $2)",
            )
            .bind(&database)
            .bind(&application)
            .fetch_one(&admin)
            .await
            .expect("inspect closed migration pool");
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("migration pool sessions close before database cleanup");
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
    sqlx::query(&format!("DROP DATABASE {database}"))
        .execute(&cluster)
        .await
        .expect("drop isolated database");
}
