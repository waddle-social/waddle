//! PG17 witnesses for the dark cooperative catalog mutation fence.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};
use waddle_server::db::physical_attestation::{
    inspect_dark_fenced_attestor, CatalogFenceExclusive, CatalogFenceIndex, CatalogFenceShared,
    CatalogMutationFenceError, DarkFencedAttestorError, DarkFencedAttestorPause,
    DarkFencedAttestorPauses, DarkPhysicalAttestationPins, PhysicalAttestationConfig,
};

use super::{dark_attestor_fixture, test_url, Fixture, CATALOG_GRANT_LOCK};

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

fn observation_pause() -> (
    DarkFencedAttestorPause,
    tokio::sync::oneshot::Receiver<i32>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (ready, observed) = tokio::sync::oneshot::channel();
    let (resume, released) = tokio::sync::oneshot::channel();
    (
        DarkFencedAttestorPause {
            ready,
            resume: released,
        },
        observed,
        resume,
    )
}

fn spawn_fenced_attestor(
    fixture: &Fixture,
    application: &str,
    key: i64,
    config: PhysicalAttestationConfig,
    pins: DarkPhysicalAttestationPins,
    pauses: DarkFencedAttestorPauses,
) -> tokio::task::JoinHandle<Result<(), DarkFencedAttestorError>> {
    let options = fixture
        .runtime_options
        .clone()
        .application_name(application);
    tokio::spawn(async move {
        inspect_dark_fenced_attestor(&options, key, WAIT, &config, pins, pauses).await
    })
}

async fn observe_pause(
    ready: tokio::sync::oneshot::Receiver<i32>,
    task: &mut tokio::task::JoinHandle<Result<(), DarkFencedAttestorError>>,
) -> i32 {
    tokio::time::timeout(Duration::from_secs(8), async {
        tokio::select! {
            pid = ready => pid.expect("stage reports the backend PID"),
            result = task => panic!("observation ended before the stage: {result:?}"),
        }
    })
    .await
    .expect("observation reaches the stage")
}

async fn assert_reader_state(pool: &PgPool, pid: i32, key: i64, in_transaction: bool) {
    let (transaction_open, lock_held): (bool, bool) = sqlx::query_as(
        "SELECT a.xact_start IS NOT NULL, EXISTS ( \
           SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid \
             AND l.locktype = 'advisory' AND l.mode = 'ShareLock' AND l.granted \
             AND l.classid = (($2::pg_catalog.int8 >> 32) & 4294967295)::pg_catalog.oid \
             AND l.objid = ($2::pg_catalog.int8 & 4294967295)::pg_catalog.oid \
             AND l.objsubid = 1) \
         FROM pg_catalog.pg_stat_activity a WHERE a.pid = $1",
    )
    .bind(pid)
    .bind(key)
    .fetch_one(pool)
    .await
    .expect("observe the exact reader transaction and fence");
    assert_eq!(transaction_open, in_transaction);
    assert!(lock_held, "the exact reader backend holds the shared fence");
}

async fn assert_idle_without_advisory_waiter(pool: &PgPool, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let idle_without_waiter: bool = sqlx::query_scalar(
                "SELECT a.state = 'idle' AND a.xact_start IS NULL AND a.backend_xmin IS NULL \
                  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
                    WHERE l.pid = a.pid AND l.locktype = 'advisory' AND NOT l.granted) \
                 FROM pg_catalog.pg_stat_activity a WHERE a.pid = $1",
            )
            .bind(pid)
            .fetch_one(pool)
            .await
            .expect("observe retrying backend");
            if idle_without_waiter {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("failed try holds no transaction, snapshot, or advisory waiter");
}

async fn wait_for_old_snapshot(pool: &PgPool, pid: i32) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_progress_create_index \
                 WHERE pid = $1 AND phase = 'waiting for old snapshots')",
            )
            .bind(pid)
            .fetch_one(pool)
            .await
            .expect("inspect concurrent reindex phase");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("REINDEX CONCURRENTLY actually waits for the old snapshot");
}

async fn reconcile_backend(pool: &PgPool, pid: i32) {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
    )
    .bind(pid)
    .fetch_one(pool)
    .await
    .expect("inspect original backend before reconciliation");
    if exists {
        let _: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
            .bind(pid)
            .fetch_one(pool)
            .await
            .expect("fence original backend");
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let gone: bool = sqlx::query_scalar(
                "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
            )
            .bind(pid)
            .fetch_one(pool)
            .await
            .expect("observe original backend termination");
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("original backend termination independently confirmed");
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

#[tokio::test]
async fn fenced_attestor_and_reindex_preserve_order_through_old_snapshot_wait() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, pins) = dark_attestor_fixture(&fixture).await;
    sqlx::query(&format!(
        "CREATE TABLE {}.fence_items (id bigint)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create maintenance fixture");
    sqlx::query(&format!(
        "CREATE INDEX fence_items_idx ON {}.fence_items (id)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create maintenance index");
    let index = CatalogFenceIndex::new(&fixture.target, "fence_items_idx").expect("index");
    let key = key();

    // Reader wins: the exclusive mutator cannot begin its protected command
    // before even the first attestor/catalog read, nor before rollback.
    let (before, before_ready, before_resume) = observation_pause();
    let (after, after_ready, after_resume) = observation_pause();
    let (rolled_back, rollback_ready, rollback_resume) = observation_pause();
    let mut reader = spawn_fenced_attestor(
        &fixture,
        "p21_fenced_reader_first",
        key,
        config.clone(),
        pins,
        DarkFencedAttestorPauses {
            before_attestor: Some(before),
            after_attestor: Some(after),
            after_rollback: Some(rolled_back),
        },
    );
    let reader_pid = observe_pause(before_ready, &mut reader).await;
    assert_reader_state(&fixture.admin, reader_pid, key, true).await;
    let maintainer_name = format!("p21_same_maint_{}", uuid::Uuid::new_v4().simple());
    let maintainer_options = fixture
        .admin_options
        .clone()
        .application_name(&maintainer_name);
    let mut maintainer = tokio::spawn(async move {
        CatalogFenceExclusive::acquire(&maintainer_options, key, WAIT).await
    });
    let waiting_pid = wait_for_idle_try(
        &fixture.admin,
        &maintainer_name,
        "SELECT pg_catalog.pg_try_advisory_lock(",
    )
    .await;
    assert_idle_without_advisory_waiter(&fixture.admin, waiting_pid).await;
    before_resume.send(()).expect("begin the attestor proof");
    assert_eq!(observe_pause(after_ready, &mut reader).await, reader_pid);
    assert_reader_state(&fixture.admin, reader_pid, key, true).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut maintainer)
            .await
            .is_err()
    );
    after_resume.send(()).expect("roll back observation");
    assert_eq!(observe_pause(rollback_ready, &mut reader).await, reader_pid);
    assert_reader_state(&fixture.admin, reader_pid, key, false).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut maintainer)
            .await
            .is_err(),
        "fence remains held after acknowledged rollback"
    );
    rollback_resume.send(()).expect("release reader fence");
    reader.await.expect("reader task").expect("fenced proof");
    let mut maintainer = maintainer
        .await
        .expect("maintainer task")
        .expect("exclusive fence after rollback and release");
    assert_eq!(maintainer.backend_pid(), waiting_pid);

    // Mutator wins: create a genuinely old repeatable-read snapshot before
    // REINDEX. PostgreSQL must report its old-snapshot wait while a second
    // attestor retries without keeping its own snapshot or advisory waiter.
    let mut old_reader = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("old-snapshot reader");
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut old_reader)
        .await
        .expect("begin old-snapshot reader");
    sqlx::query(&format!(
        "SELECT count(*) FROM {}.fence_items",
        fixture.target
    ))
    .fetch_one(&mut old_reader)
    .await
    .expect("establish old snapshot");
    let maintainer_pid = maintainer.backend_pid();
    let reindex = tokio::spawn(async move {
        let result = maintainer.reindex(&index).await;
        (maintainer, result)
    });
    wait_for_old_snapshot(&fixture.admin, maintainer_pid).await;
    let reader_name = format!("p21_same_reader_{}", uuid::Uuid::new_v4().simple());
    let (after, after_ready, after_resume) = observation_pause();
    let mut second_reader = spawn_fenced_attestor(
        &fixture,
        &reader_name,
        key,
        config,
        pins,
        DarkFencedAttestorPauses {
            after_attestor: Some(after),
            ..Default::default()
        },
    );
    let second_pid = wait_for_idle_try(
        &fixture.admin,
        &reader_name,
        "SELECT pg_catalog.pg_try_advisory_lock_shared(",
    )
    .await;
    assert_idle_without_advisory_waiter(&fixture.admin, second_pid).await;
    wait_for_old_snapshot(&fixture.admin, maintainer_pid).await;
    sqlx::query("ROLLBACK")
        .execute(&mut old_reader)
        .await
        .expect("end the old snapshot");
    let (maintainer, result) = reindex.await.expect("reindex task");
    result.expect("fenced REINDEX completes after the old snapshot ends");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second_reader)
            .await
            .is_err(),
        "exclusive fence still blocks the reader after REINDEX"
    );
    maintainer.release().await.expect("release exclusive fence");
    assert_eq!(
        observe_pause(after_ready, &mut second_reader).await,
        second_pid
    );
    assert_reader_state(&fixture.admin, second_pid, key, true).await;
    after_resume.send(()).expect("complete second observation");
    second_reader
        .await
        .expect("second reader task")
        .expect("same retrying backend attests after maintenance");
    drop(old_reader);
    fixture.cleanup().await;
}

#[tokio::test]
async fn fenced_attestor_does_not_enforce_uncooperative_mutators() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, pins) = dark_attestor_fixture(&fixture).await;
    sqlx::query(&format!(
        "CREATE TABLE {}.bypass_items (id bigint)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create bypass fixture");
    sqlx::query(&format!(
        "CREATE INDEX bypass_items_idx ON {}.bypass_items (id)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create bypass index");
    let key = key();
    let (after, ready, resume) = observation_pause();
    let mut reader = spawn_fenced_attestor(
        &fixture,
        "p21_fenced_bypass",
        key,
        config,
        pins,
        DarkFencedAttestorPauses {
            after_attestor: Some(after),
            ..Default::default()
        },
    );
    let pid = observe_pause(ready, &mut reader).await;
    assert_reader_state(&fixture.admin, pid, key, true).await;
    assert!(matches!(
        CatalogFenceExclusive::acquire(&fixture.admin_options, key, Duration::from_millis(100))
            .await,
        Err(CatalogMutationFenceError::Contended)
    ));
    let index_oid = || {
        sqlx::query_scalar::<_, i64>(
            "SELECT c.oid::bigint FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = 'bypass_items_idx'",
        )
        .bind(&fixture.target)
        .fetch_one(&fixture.admin)
    };
    let before = index_oid().await.expect("original index OID");
    sqlx::query(&format!(
        "REINDEX INDEX CONCURRENTLY {}.bypass_items_idx",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("non-cooperating privileged mutator bypasses the advisory fence");
    let after = index_oid().await.expect("replacement index OID");
    assert_ne!(
        before, after,
        "uncooperative REINDEX changed catalog identity"
    );
    assert_reader_state(&fixture.admin, pid, key, true).await;
    resume.send(()).expect("complete observation");
    reader
        .await
        .expect("reader task")
        .expect("observation ends");
    fixture.cleanup().await;
}

#[tokio::test]
async fn fenced_attestor_cancellation_and_lost_backend_fail_closed() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, pins) = dark_attestor_fixture(&fixture).await;
    let key = key();
    let (after, ready, resume) = observation_pause();
    let mut reader = spawn_fenced_attestor(
        &fixture,
        "p21_fenced_cancel",
        key,
        config.clone(),
        pins,
        DarkFencedAttestorPauses {
            after_attestor: Some(after),
            ..Default::default()
        },
    );
    let pid = observe_pause(ready, &mut reader).await;
    assert_reader_state(&fixture.admin, pid, key, true).await;
    reader.abort();
    assert!(reader
        .await
        .expect_err("cancelled task has no successful result")
        .is_cancelled());
    drop(resume);
    // A cancelled client is not a backend fence. Observe or terminate that
    // exact original backend before treating its transaction and lock as gone.
    reconcile_backend(&fixture.admin, pid).await;
    let replacement = CatalogFenceExclusive::acquire(&fixture.admin_options, key, WAIT)
        .await
        .expect("exclusive fence after original backend reconciliation");
    replacement.release().await.expect("release replacement");

    let (after, ready, resume) = observation_pause();
    let mut reader = spawn_fenced_attestor(
        &fixture,
        "p21_fenced_lost",
        key,
        config.clone(),
        pins,
        DarkFencedAttestorPauses {
            after_attestor: Some(after),
            ..Default::default()
        },
    );
    let pid = observe_pause(ready, &mut reader).await;
    assert_reader_state(&fixture.admin, pid, key, true).await;
    let terminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&fixture.admin)
        .await
        .expect("terminate fenced backend");
    assert!(terminated);
    resume.send(()).expect("resume lost-backend observation");
    assert!(matches!(
        reader.await.expect("lost-backend task"),
        Err(DarkFencedAttestorError::RollbackUnconfirmed { backend_pid }) if backend_pid == pid
    ));
    reconcile_backend(&fixture.admin, pid).await;

    // A stale independent pin must reach the attestor refusal path, which
    // then leaves no fence behind after the observation returns.
    let mut bad_pins = pins;
    bad_pins.attestor_function_oid = 1;
    let (before, ready, resume) = observation_pause();
    let mut reader = spawn_fenced_attestor(
        &fixture,
        "p21_fenced_bad_pin",
        key,
        config,
        bad_pins,
        DarkFencedAttestorPauses {
            before_attestor: Some(before),
            ..Default::default()
        },
    );
    let pid = observe_pause(ready, &mut reader).await;
    assert_reader_state(&fixture.admin, pid, key, true).await;
    resume.send(()).expect("run proof with a stale pin");
    assert!(matches!(
        reader.await.expect("stale-pin task"),
        Err(DarkFencedAttestorError::Attestation(_))
    ));
    let replacement = CatalogFenceExclusive::acquire(&fixture.admin_options, key, WAIT)
        .await
        .expect("exclusive fence after rejected proof rolls back");
    replacement.release().await.expect("release replacement");
    fixture.cleanup().await;
}
