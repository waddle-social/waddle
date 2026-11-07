//! Restricted PostgreSQL attachment for the call-teardown outbox.

use super::{
    attested_pool, install, proxy_runtime_options, test_url, url_with_schema, Database,
    DatabaseConfig, DatabaseDriver, DatabaseError, Duration, Fixture, CATALOG_GRANT_LOCK,
};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, PgPool};
use waddle_server::call_teardown_outbox::{
    CallTeardownOutboxError, CallTeardownOutboxStore, CallTeardownRuntimeAttachError,
};
use waddle_server::db::lineage::PgIdentity;
use waddle_server::db::physical_attestation::{
    PhysicalAttestationConfig, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

const OWNER_GATE: i64 = 6_841_445_497_037_937_997;

const LEGACY_SCHEMA: &str =
    include_str!("../../src/call_teardown_outbox/tests/fixtures/pre_displaced_retirement.sql");

async fn run_admin(fixture: &Fixture, statement: &str) {
    sqlx::query(statement)
        .execute(&fixture.admin)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

fn owner_url(fixture: &Fixture, owner: &str, migrator: &str) -> String {
    let mut url = url::Url::parse(&fixture.base_url).expect("base URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "user" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("user", migrator)
        .append_pair(
            "options",
            &format!("-c role={owner} -c search_path={},pg_temp", fixture.target),
        );
    url.to_string()
}

fn node_identity() -> SharedNodeIdentity {
    SharedNodeIdentity::new(NodeIdentity::new("call-teardown-test", "epoch-1"))
}

fn runtime_error(error: CallTeardownOutboxError) -> CallTeardownRuntimeAttachError {
    let CallTeardownOutboxError::RuntimeAttach(error) = error else {
        panic!("expected runtime attach error, got {error}");
    };
    error
}

fn legacy_postgres_schema() -> String {
    LEGACY_SCHEMA
        .replace(
            "thread_started_at_ms INTEGER",
            "thread_started_at_ms BIGINT",
        )
        .replace("thread_ended_at_ms INTEGER", "thread_ended_at_ms BIGINT")
        .replace("next_attempt_at_ms INTEGER", "next_attempt_at_ms BIGINT")
        .replace("claimed_at_ms INTEGER", "claimed_at_ms BIGINT")
        .replace("created_at_ms INTEGER", "created_at_ms BIGINT")
        .replace("updated_at_ms INTEGER", "updated_at_ms BIGINT")
}

fn named_url(url: &str, application: &str) -> String {
    let mut url = url::Url::parse(url).expect("PostgreSQL URL");
    url.query_pairs_mut()
        .append_pair("application_name", application);
    url.to_string()
}

async fn wait_retry(admin: &PgPool, application: &str) -> i32 {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_stat_activity \
                 WHERE application_name = $1 AND state = 'idle' AND query = 'ROLLBACK' \
                   AND xact_start IS NULL AND backend_xmin IS NULL",
            )
            .bind(application)
            .fetch_optional(admin)
            .await
            .expect("observe acknowledged retry rollback");
            if let Some(pid) = pid {
                // The attested checkout hook also issues ROLLBACK before
                // lending the connection. The acquisition retry has a
                // 20 ms backoff; require an idle interval so a transient
                // hook query is not mistaken for the retry boundary.
                tokio::time::sleep(Duration::from_millis(6)).await;
                let still_idle: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                     WHERE pid = $1 AND state = 'idle' AND query = 'ROLLBACK' \
                       AND xact_start IS NULL AND backend_xmin IS NULL)",
                )
                .bind(pid)
                .fetch_one(admin)
                .await
                .expect("confirm retry backoff");
                if still_idle {
                    break pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("entry rolls back before retrying")
}

async fn wait_relation_waiter(
    admin: &PgPool,
    application: &str,
    relation: &str,
    mode: &str,
) -> i32 {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT a.pid FROM pg_catalog.pg_stat_activity a \
                 JOIN pg_catalog.pg_locks l ON l.pid = a.pid \
                 WHERE a.application_name = $1 AND l.relation = $2::regclass \
                   AND l.mode = $3 AND NOT l.granted",
            )
            .bind(application)
            .bind(relation)
            .bind(mode)
            .fetch_optional(admin)
            .await
            .expect("observe relation waiter");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("entry reaches the relation lock")
}

async fn assert_advisory_lock(admin: &PgPool, pid: i32, key: i64, mode: &str) {
    let held: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
         WHERE pid = $1 AND locktype = 'advisory' AND granted AND mode = $2 \
           AND classid = (($3::bigint >> 32) & 4294967295)::oid \
           AND objid = ($3::bigint & 4294967295)::oid AND objsubid = 1)",
    )
    .bind(pid)
    .bind(mode)
    .bind(key)
    .fetch_one(admin)
    .await
    .expect("inspect backend advisory lock");
    assert!(held, "backend {pid} must hold {mode} on {key}");
}

async fn assert_retry_releases_catalog(fixture: &Fixture, application: &str) {
    wait_retry(&fixture.admin, application).await;
    let mut observer = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("catalog observer");
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let acquired: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock($1)")
                .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                .fetch_one(&mut observer)
                .await
                .expect("try catalog-X");
            if acquired {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("a failed lower gate releases catalog");
    let pid = wait_retry(&fixture.admin, application).await;
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_locks \
         WHERE pid = $1 AND locktype = 'advisory' AND granted",
    )
    .bind(pid)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect rolled-back attempt");
    assert_eq!(held, 0);
    let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(&mut observer)
        .await
        .expect("release observer catalog fence");
    assert!(unlocked);
}

async fn wait_backend_exit(admin: &PgPool, pid: i32) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
            )
            .bind(pid)
            .fetch_one(admin)
            .await
            .expect("inspect cancelled backend");
            if !exists {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("cancelled checkout is discarded");
}

async fn assert_replacement_checkout(db: &Database, cancelled_pid: i32) {
    let replacement = checkout_pid(db).await;
    assert_ne!(replacement, i64::from(cancelled_pid));
}

async fn checkout_pid(db: &Database) -> i64 {
    let connection = db.guard().await.expect("replacement checkout");
    let mut rows = connection
        .query("SELECT pg_catalog.pg_backend_pid()::bigint", ())
        .await
        .expect("replacement backend PID");
    rows.next()
        .await
        .expect("replacement PID row")
        .expect("replacement PID exists")
        .get(0)
        .expect("decode replacement PID")
}

fn spawn_owner(
    db: Database,
    identity: Option<PgIdentity>,
) -> tokio::task::JoinHandle<Result<(), CallTeardownOutboxError>> {
    tokio::spawn(async move {
        match identity {
            Some(identity) => CallTeardownOutboxStore::initialize_owner(&db, &identity).await,
            None => CallTeardownOutboxStore::try_new(db).await.map(|_| ()),
        }
    })
}

async fn exercise_owner_entry(
    fixture: &Fixture,
    db: &Database,
    identity: Option<PgIdentity>,
    application: &str,
) {
    let relation = format!("{}.call_teardown_outbox", fixture.target);
    let mut blocker = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("owner fence blocker");
    sqlx::query("BEGIN")
        .execute(&mut blocker)
        .await
        .expect("begin blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_lock_shared($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut blocker)
        .await
        .expect("hold catalog-S");
    sqlx::query(&format!("LOCK TABLE {relation} IN ACCESS SHARE MODE"))
        .execute(&mut blocker)
        .await
        .expect("hold relation barrier");
    let task = spawn_owner(db.clone(), identity.clone());
    let pid = wait_retry(&fixture.admin, application).await;
    assert!(!task.is_finished());
    let early_locks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_locks \
         WHERE pid = $1 AND granted AND (locktype = 'advisory' OR relation = $2::regclass)",
    )
    .bind(pid)
    .bind(&relation)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect pre-fence locks");
    assert_eq!(
        early_locks, 0,
        "catalog contention precedes store and relation locks"
    );
    let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock_shared($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(&mut blocker)
        .await
        .expect("release catalog-S");
    assert!(unlocked);
    let pid = wait_relation_waiter(
        &fixture.admin,
        application,
        &relation,
        "AccessExclusiveLock",
    )
    .await;
    assert_advisory_lock(
        &fixture.admin,
        pid,
        CATALOG_MUTATION_ADVISORY_LOCK_KEY,
        "ExclusiveLock",
    )
    .await;
    assert_advisory_lock(&fixture.admin, pid, OWNER_GATE, "ExclusiveLock").await;
    task.abort();
    assert!(task.await.expect_err("cancel owner entry").is_cancelled());
    assert_replacement_checkout(db, pid).await;
    sqlx::query("ROLLBACK")
        .execute(&mut blocker)
        .await
        .expect("release relation barrier");
    wait_backend_exit(&fixture.admin, pid).await;
    let partial: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute \
         WHERE attrelid = $1::regclass AND attname = 'unbound_occupant' AND NOT attisdropped)",
    )
    .bind(&relation)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect cancelled upgrade");
    assert!(!partial, "cancelled owner leaves no partial upgrade");

    let mut relation_holder = fixture.admin.begin().await.expect("relation holder");
    sqlx::query(&format!("LOCK TABLE {relation} IN ACCESS SHARE MODE"))
        .execute(&mut *relation_holder)
        .await
        .expect("hold conflicting relation lock");
    let task = spawn_owner(db.clone(), identity.clone());
    wait_relation_waiter(
        &fixture.admin,
        application,
        &relation,
        "AccessExclusiveLock",
    )
    .await;
    let error = tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .expect("owner relation wait is bounded")
        .expect("join timed-out owner")
        .expect_err("conflicting relation lock must time out");
    assert!(matches!(
        error,
        CallTeardownOutboxError::Database(DatabaseError::Internal(sqlx::Error::Database(error)))
            if error.code().as_deref() == Some("55P03")
    ));
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let acquired: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                    .fetch_one(&mut *relation_holder)
                    .await
                    .expect("try catalog-X after relation timeout");
            if acquired {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("timed-out owner releases catalog-X while relation remains held");
    relation_holder
        .rollback()
        .await
        .expect("release relation holder");

    let mut lower = fixture.admin.begin().await.expect("lower-gate blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(OWNER_GATE)
        .execute(&mut *lower)
        .await
        .expect("hold owner gate");
    let task = spawn_owner(db.clone(), identity);
    assert_retry_releases_catalog(fixture, application).await;
    assert!(!task.is_finished());
    lower.rollback().await.expect("release owner gate");
    task.await
        .expect("join owner entry")
        .expect("owner commits");
    let upgraded: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute \
         WHERE attrelid = $1::regclass AND attname = 'unbound_occupant' AND NOT attisdropped)",
    )
    .bind(&relation)
    .fetch_one(&fixture.admin)
    .await
    .expect("observe committed upgrade");
    assert!(upgraded, "another backend sees the completed upgrade");
}

#[tokio::test]
async fn raw_postgres_call_teardown_initialization_is_fenced_and_atomic() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let version: i32 =
        sqlx::query_scalar("SELECT pg_catalog.current_setting('server_version_num')::integer")
            .fetch_one(&fixture.admin)
            .await
            .expect("PostgreSQL version");
    assert!((170_000..180_000).contains(&version), "PG17 is required");
    let application = format!("ct_raw_{}", uuid::Uuid::new_v4().simple());
    let mut config = DatabaseConfig::new(
        DatabaseDriver::Postgres,
        named_url(
            &url_with_schema(&fixture.base_url, &fixture.target),
            &application,
        ),
    );
    config.pool_size = 1;
    let db = Database::from_config("raw call teardown owner", &config)
        .await
        .expect("raw PostgreSQL owner");
    let connection = db.guard().await.expect("raw owner connection");
    connection
        .execute(&legacy_postgres_schema(), ())
        .await
        .expect("legacy schema");
    connection.execute(
        "INSERT INTO call_teardown_outbox \
         (intent_id, call_id, identity, action, status, created_at_ms, updated_at_ms) \
         VALUES ('retained', 'call', 'alice@example.test/device', 'remove_participant', 'queued', 1, 1)",
        (),
    ).await.expect("seed legacy row");
    drop(connection);
    exercise_owner_entry(&fixture, &db, None, &application).await;
    let (retained, indexes): (i64, i64) = sqlx::query_as(&format!(
        "SELECT (SELECT count(*) FROM {}.call_teardown_outbox WHERE intent_id = 'retained'), \
         (SELECT count(*) FROM pg_catalog.pg_indexes WHERE schemaname = $1 \
          AND indexname IN ('idx_call_teardown_outbox_status_due', 'idx_call_teardown_outbox_dedupe'))",
        fixture.target
    ))
    .bind(&fixture.target)
    .fetch_one(&fixture.admin)
    .await
    .expect("observe committed row and indexes");
    assert_eq!((retained, indexes), (1, 2));

    run_admin(
        &fixture,
        &format!("DROP TABLE {}.call_teardown_outbox", fixture.target),
    )
    .await;
    run_admin(&fixture, &format!("CREATE TABLE {}.call_teardown_outbox (intent_id TEXT PRIMARY KEY, call_id INTEGER NOT NULL)", fixture.target)).await;
    assert!(CallTeardownOutboxStore::try_new(db.clone()).await.is_err());
    let columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_attribute \
         WHERE attrelid = $1::regclass AND attnum > 0 AND NOT attisdropped",
    )
    .bind(format!("{}.call_teardown_outbox", fixture.target))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect failed raw upgrade");
    assert_eq!(columns, 2, "failed raw upgrade rolls back earlier columns");
    drop(db);
    fixture.cleanup().await;
}

async fn exercise_reader_entry(
    fixture: &Fixture,
    runtime: &Database,
    node_identity: &SharedNodeIdentity,
    application: &str,
) {
    let relation = format!("{}.call_teardown_outbox", fixture.target);
    let mut blocker = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("reader fence blocker");
    sqlx::query("BEGIN")
        .execute(&mut blocker)
        .await
        .expect("begin blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut blocker)
        .await
        .expect("hold catalog-X");
    sqlx::query(&format!("LOCK TABLE {relation} IN ACCESS EXCLUSIVE MODE"))
        .execute(&mut blocker)
        .await
        .expect("hold relation barrier");
    let task = tokio::spawn({
        let runtime = runtime.clone();
        let node_identity = node_identity.clone();
        async move { CallTeardownOutboxStore::attach_existing(runtime, node_identity).await }
    });
    let pid = wait_retry(&fixture.admin, application).await;
    assert!(!task.is_finished());
    let early_locks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_locks \
         WHERE pid = $1 AND granted AND (locktype = 'advisory' OR relation = $2::regclass)",
    )
    .bind(pid)
    .bind(&relation)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect reader pre-fence locks");
    assert_eq!(
        early_locks, 0,
        "reader reaches catalog before the store and relation"
    );
    let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(&mut blocker)
        .await
        .expect("release catalog-X");
    assert!(unlocked);
    let pid = wait_relation_waiter(&fixture.admin, application, &relation, "AccessShareLock").await;
    assert_advisory_lock(
        &fixture.admin,
        pid,
        CATALOG_MUTATION_ADVISORY_LOCK_KEY,
        "ShareLock",
    )
    .await;
    assert_advisory_lock(&fixture.admin, pid, OWNER_GATE, "ShareLock").await;
    let no_snapshot: bool = sqlx::query_scalar(
        "SELECT backend_xmin IS NULL FROM pg_catalog.pg_stat_activity WHERE pid = $1",
    )
    .bind(pid)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect reader snapshot while relation is blocked");
    assert!(
        no_snapshot,
        "reader must not retain a repeatable-read snapshot"
    );
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    assert_replacement_checkout(runtime, pid).await;
    sqlx::query("ROLLBACK")
        .execute(&mut blocker)
        .await
        .expect("release relation barrier");
    wait_backend_exit(&fixture.admin, pid).await;

    let mut lower = fixture
        .admin
        .begin()
        .await
        .expect("reader lower gate blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(OWNER_GATE)
        .execute(&mut *lower)
        .await
        .expect("hold owner gate");
    let task = tokio::spawn({
        let runtime = runtime.clone();
        let node_identity = node_identity.clone();
        async move { CallTeardownOutboxStore::attach_existing(runtime, node_identity).await }
    });
    assert_retry_releases_catalog(fixture, application).await;
    assert!(!task.is_finished());
    lower.rollback().await.expect("release owner gate");
    task.await
        .expect("join reader entry")
        .expect("reader attaches");
}

async fn hold_reader_rollback_reply(
    listener: tokio::net::TcpListener,
    upstream: PgConnectOptions,
    held: tokio::sync::oneshot::Sender<()>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (client, _) = listener.accept().await?;
    let server = crate::postgres_proxy::connect_local_postgres(&upstream).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let forward = async move {
        tokio::io::copy(&mut client_read, &mut server_write)
            .await
            .map(|_| ())
    };
    let backward = async move {
        let mut began = false;
        let mut held = Some(held);
        loop {
            let tag = server_read.read_u8().await?;
            let length = server_read.read_u32().await?;
            if !(4..=1_048_576).contains(&length) {
                return Err(std::io::Error::other("invalid backend frame"));
            }
            let mut body = vec![0; (length - 4) as usize];
            server_read.read_exact(&mut body).await?;
            if tag == b'C' && body == b"BEGIN\0" {
                began = true;
            }
            if began && tag == b'C' && body == b"ROLLBACK\0" {
                held.take()
                    .expect("first rollback")
                    .send(())
                    .map_err(|_| std::io::Error::other("holder dropped"))?;
                std::future::pending::<()>().await;
            }
            client_write.write_u8(tag).await?;
            client_write.write_u32(length).await?;
            client_write.write_all(&body).await?;
        }
    };
    tokio::select! { result = forward => result, result = backward => result }
}

async fn exercise_reader_rollback(
    fixture: &Fixture,
    attestation: &PhysicalAttestationConfig,
    node_identity: &SharedNodeIdentity,
) {
    let single_pool = attested_pool(
        fixture.runtime_options.clone(),
        attestation.clone(),
        1,
        Duration::from_secs(3),
    )
    .expect("single-connection attested pool");
    let single = waddle_server::test_support::database_from_attested_postgres_pool(
        "call teardown rollback reuse",
        single_pool,
    );
    let before = checkout_pid(&single).await;
    CallTeardownOutboxStore::attach_existing(single.clone(), node_identity.clone())
        .await
        .expect("reader rollback acknowledged");
    let after = checkout_pid(&single).await;
    assert_eq!(before, after, "acknowledged rollback returns the checkout");
    drop(single);

    // Fault injection is restricted to a local plaintext PostgreSQL proxy.
    if crate::postgres_proxy::ensure_local_target(&fixture.admin_options).is_err() {
        return;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind rollback proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let (held, waiting) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(hold_reader_rollback_reply(
        listener,
        fixture.admin_options.clone(),
        held,
    ));
    let proxied = attested_pool(
        proxy_runtime_options(fixture, port).application_name("call-teardown-lost-rollback"),
        attestation.clone(),
        1,
        Duration::from_secs(3),
    )
    .expect("proxied attested pool");
    let proxied = waddle_server::test_support::database_from_attested_postgres_pool(
        "call teardown lost rollback reply",
        proxied,
    );
    let task = tokio::spawn({
        let node_identity = node_identity.clone();
        async move { CallTeardownOutboxStore::attach_existing(proxied, node_identity).await }
    });
    tokio::time::timeout(Duration::from_secs(8), waiting)
        .await
        .expect("reader reaches rollback")
        .expect("proxy holds rollback reply");
    let error = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("rollback acknowledgement is bounded")
        .expect("join reader")
        .err()
        .expect("unacknowledged rollback must not attach");
    assert!(matches!(
        runtime_error(error),
        CallTeardownRuntimeAttachError::Database(DatabaseError::CatalogMutationRollbackUnconfirmed)
    ));
    proxy.abort();
    let _ = proxy.await;
}

#[tokio::test]
async fn call_teardown_attaches_only_to_prepared_restricted_main_pool() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let version: i32 =
        sqlx::query_scalar("SELECT pg_catalog.current_setting('server_version_num')::integer")
            .fetch_one(&fixture.admin)
            .await
            .expect("PostgreSQL version");
    assert!((170_000..180_000).contains(&version), "PG17 is required");
    let owner = format!("ct_owner_{}", uuid::Uuid::new_v4().simple());
    let migrator = format!("ct_migrator_{}", uuid::Uuid::new_v4().simple());
    run_admin(&fixture, &format!("CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS")).await;
    run_admin(&fixture, &format!("CREATE ROLE {migrator} LOGIN")).await;
    run_admin(
        &fixture,
        &format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
    )
    .await;
    run_admin(
        &fixture,
        &format!("ALTER SCHEMA {} OWNER TO {owner}", fixture.target),
    )
    .await;
    run_admin(
        &fixture,
        &format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
    )
    .await;
    run_admin(
        &fixture,
        &format!(
            "ALTER ROLE {} SET default_transaction_isolation = 'repeatable read'",
            fixture.runtime
        ),
    )
    .await;

    let pg_identity: PgIdentity = fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("lineage carries physical identity")
        .clone();
    let owner_application = format!("ct_strict_{}", uuid::Uuid::new_v4().simple());
    let reader_application = format!("ct_reader_{}", uuid::Uuid::new_v4().simple());
    let mut owner_config = DatabaseConfig::new(
        DatabaseDriver::Postgres,
        named_url(&owner_url(&fixture, &owner, &migrator), &owner_application),
    );
    owner_config.pool_size = 1;
    let owner_db = Database::from_config("call teardown trusted owner", &owner_config)
        .await
        .expect("trusted owner database");

    let attestation = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install physical attestor");
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "call teardown main and ingress",
        attested_pool(
            fixture
                .runtime_options
                .clone()
                .application_name(&reader_application),
            attestation.clone(),
            2,
            Duration::from_secs(3),
        )
        .expect("restricted attested pool"),
        1,
        1,
    )
    .await
    .expect("main and ingress lanes");
    let runtime = pool.global().clone();
    let node_identity = node_identity();

    let error = CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
        .await
        .err()
        .expect("missing owner schema must fail closed");
    assert!(matches!(
        runtime_error(error),
        CallTeardownRuntimeAttachError::Relation(_)
    ));
    let missing: Option<String> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
        .bind(format!("{}.call_teardown_outbox", fixture.target))
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect missing relation");
    assert!(
        missing.is_none(),
        "runtime attachment must not create tables"
    );
    assert!(matches!(
        CallTeardownOutboxStore::try_new(runtime.clone()).await,
        Err(CallTeardownOutboxError::OwnerPreparationRequired)
    ));

    let mut wrong_identity = pg_identity.clone();
    wrong_identity.database.oid += 1;
    assert!(
        CallTeardownOutboxStore::initialize_owner(&owner_db, &wrong_identity)
            .await
            .is_err()
    );
    let still_missing: Option<String> =
        sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
            .bind(format!("{}.call_teardown_outbox", fixture.target))
            .fetch_one(&fixture.admin)
            .await
            .expect("reinspect relation after wrong identity");
    assert!(
        still_missing.is_none(),
        "wrong identity must not mutate schema"
    );

    // A partial owner upgrade must roll back all preceding column additions.
    owner_db
        .guard()
        .await
        .expect("owner connection")
        .execute(
            "CREATE TABLE call_teardown_outbox (intent_id TEXT PRIMARY KEY, call_id INTEGER NOT NULL)",
            (),
        )
        .await
        .expect("malformed owner fixture");
    assert!(
        CallTeardownOutboxStore::initialize_owner(&owner_db, &pg_identity)
            .await
            .is_err()
    );
    let malformed_column_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = 'call_teardown_outbox' \
           AND a.attnum > 0 AND NOT a.attisdropped",
    )
    .bind(&fixture.target)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect rolled-back columns");
    assert_eq!(malformed_column_count, 2, "owner preparation is atomic");
    run_admin(
        &fixture,
        &format!("DROP TABLE {}.call_teardown_outbox", fixture.target),
    )
    .await;

    // Prepare from the supported pre-#1732 shape and retain an existing row.
    let old_schema = legacy_postgres_schema();
    owner_db
        .guard()
        .await
        .expect("owner connection")
        .execute(&old_schema, ())
        .await
        .expect("create legacy outbox schema");
    owner_db
        .guard()
        .await
        .expect("owner connection")
        .execute(
            "INSERT INTO call_teardown_outbox \
             (intent_id, call_id, identity, action, status, created_at_ms, updated_at_ms) \
             VALUES ('retained-intent', 'retained-call', 'alice@example.test/device', \
                     'remove_participant', 'queued', 1, 1)",
            (),
        )
        .await
        .expect("seed legacy durable intent");
    exercise_owner_entry(
        &fixture,
        &owner_db,
        Some(pg_identity.clone()),
        &owner_application,
    )
    .await;
    let retained: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*)::bigint FROM {}.call_teardown_outbox WHERE intent_id = 'retained-intent'",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect preserved intent");
    assert_eq!(retained, 1, "schema upgrade preserves outbox rows");
    run_admin(
        &fixture,
        &format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.call_teardown_outbox TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;

    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(pool.ingress(), node_identity.clone())
                .await
                .err()
                .expect("ingress lane cannot attach")
        ),
        CallTeardownRuntimeAttachError::MainLaneRequired
    ));
    let attached = CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
        .await
        .expect("restricted main lane attaches");
    assert_eq!(
        attached
            .queue_stats()
            .await
            .expect("functional store can read the outbox")
            .queued_count,
        1
    );
    exercise_reader_entry(&fixture, &runtime, &node_identity, &reader_application).await;
    exercise_reader_rollback(&fixture, &attestation, &node_identity).await;
    assert!(runtime
        .guard()
        .await
        .expect("runtime connection")
        .execute("CREATE TABLE runtime_ddl_probe (id integer)", ())
        .await
        .is_err());

    run_admin(
        &fixture,
        &format!(
            "CREATE INDEX call_teardown_unexpected_idx ON {}.call_teardown_outbox (created_at_ms)",
            fixture.target
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("extra index must be refused")
        ),
        CallTeardownRuntimeAttachError::InvalidIndexes
    ));
    run_admin(
        &fixture,
        &format!("DROP INDEX {}.call_teardown_unexpected_idx", fixture.target),
    )
    .await;

    run_admin(
        &fixture,
        &format!(
            "ALTER TABLE {}.call_teardown_outbox DROP CONSTRAINT call_teardown_outbox_target_check",
            fixture.target
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("missing target check must be refused")
        ),
        CallTeardownRuntimeAttachError::InvalidConstraints
    ));
    CallTeardownOutboxStore::initialize_owner(&owner_db, &pg_identity)
        .await
        .expect("owner upgrade restores the supported check contract");

    run_admin(
        &fixture,
        &format!(
            "CREATE TABLE {}.call_teardown_inbound_ref (intent_id text \
             REFERENCES {}.call_teardown_outbox(intent_id) ON DELETE CASCADE)",
            fixture.target, fixture.target
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("inbound cascade foreign key must be refused")
        ),
        CallTeardownRuntimeAttachError::InvalidTable
    ));
    run_admin(
        &fixture,
        &format!("DROP TABLE {}.call_teardown_inbound_ref", fixture.target),
    )
    .await;

    run_admin(
        &fixture,
        &format!(
            "GRANT TRUNCATE ON {}.call_teardown_outbox TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("runtime truncate privilege must be refused")
        ),
        CallTeardownRuntimeAttachError::UnsafeRuntimePrivileges
    ));
    run_admin(
        &fixture,
        &format!(
            "REVOKE TRUNCATE ON {}.call_teardown_outbox FROM {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    run_admin(
        &fixture,
        &format!(
            "GRANT CREATE ON SCHEMA {} TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("runtime schema-create privilege must be refused")
        ),
        CallTeardownRuntimeAttachError::UnsafeRuntimeRole
    ));
    run_admin(
        &fixture,
        &format!(
            "REVOKE CREATE ON SCHEMA {} FROM {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    run_admin(
        &fixture,
        &format!(
            "GRANT SELECT ON {}.call_teardown_outbox TO {}",
            fixture.target, fixture.outsider
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("unexpected relation grant must be refused")
        ),
        CallTeardownRuntimeAttachError::UnsafeRuntimePrivileges
    ));
    run_admin(
        &fixture,
        &format!(
            "REVOKE SELECT ON {}.call_teardown_outbox FROM {}",
            fixture.target, fixture.outsider
        ),
    )
    .await;

    // The reader must see a change committed while it waits for catalog-S.
    let mut ddl = fixture.admin.begin().await.expect("owner DDL transaction");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *ddl)
        .await
        .expect("hold catalog-X across the shape change");
    sqlx::query(&format!(
        "ALTER TABLE {}.call_teardown_outbox ALTER COLUMN call_id DROP NOT NULL",
        fixture.target
    ))
    .execute(&mut *ddl)
    .await
    .expect("hold a shape-changing owner DDL lock");
    let attach_task = tokio::spawn({
        let runtime = runtime.clone();
        let node_identity = node_identity.clone();
        async move { CallTeardownOutboxStore::attach_existing(runtime, node_identity).await }
    });
    wait_retry(&fixture.admin, &reader_application).await;
    assert!(
        !attach_task.is_finished(),
        "attachment must wait for conflicting owner DDL"
    );
    ddl.commit().await.expect("commit shape change");
    let race_error = runtime_error(
        attach_task
            .await
            .expect("attachment task")
            .err()
            .expect("attachment sees the committed invalid shape"),
    );
    assert!(matches!(
        race_error,
        CallTeardownRuntimeAttachError::InvalidColumn("call_id")
    ));
    run_admin(
        &fixture,
        &format!(
            "ALTER TABLE {}.call_teardown_outbox ALTER COLUMN call_id SET NOT NULL",
            fixture.target
        ),
    )
    .await;
    CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
        .await
        .expect("restored shape attaches");

    // A role change committed after checkout must be checked by the actual
    // attachment transaction after it obtains catalog-S.
    let mut role_change = fixture.admin.begin().await.expect("role change");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *role_change)
        .await
        .expect("hold catalog-X for role change");
    let attach_task = tokio::spawn({
        let runtime = runtime.clone();
        let node_identity = node_identity.clone();
        async move { CallTeardownOutboxStore::attach_existing(runtime, node_identity).await }
    });
    wait_retry(&fixture.admin, &reader_application).await;
    sqlx::query(&format!("ALTER ROLE {} CREATEROLE", fixture.runtime))
        .execute(&mut *role_change)
        .await
        .expect("invalidate runtime role");
    role_change.commit().await.expect("commit role change");
    assert!(matches!(
        runtime_error(
            attach_task
                .await
                .expect("join role race")
                .err()
                .expect("reject role drift")
        ),
        CallTeardownRuntimeAttachError::UnsafeRuntimeRole
    ));
    run_admin(
        &fixture,
        &format!("ALTER ROLE {} NOCREATEROLE", fixture.runtime),
    )
    .await;

    // Preserve the relation name and stored lineage but replace the schema
    // identity while both public entries wait. Both must reject the new OID.
    let old_schema = format!("{}_old", fixture.target);
    let mut replacement = fixture.admin.begin().await.expect("schema replacement");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *replacement)
        .await
        .expect("hold catalog-X for schema replacement");
    let attach_task = tokio::spawn({
        let runtime = runtime.clone();
        let node_identity = node_identity.clone();
        async move { CallTeardownOutboxStore::attach_existing(runtime, node_identity).await }
    });
    wait_retry(&fixture.admin, &reader_application).await;
    let owner_task = spawn_owner(owner_db.clone(), Some(pg_identity.clone()));
    wait_retry(&fixture.admin, &owner_application).await;
    for statement in [
        format!("ALTER SCHEMA {} RENAME TO {old_schema}", fixture.target),
        format!("CREATE SCHEMA {} AUTHORIZATION {owner}", fixture.target),
        format!("GRANT USAGE ON SCHEMA {} TO {}, {}", fixture.target, fixture.runtime, fixture.definer),
        format!("CREATE TABLE {}._lineage AS TABLE {old_schema}._lineage", fixture.target),
        format!("GRANT SELECT ON {}._lineage TO {}", fixture.target, fixture.definer),
        format!("CREATE TABLE {}.call_teardown_outbox (LIKE {old_schema}.call_teardown_outbox INCLUDING ALL)", fixture.target),
        format!("GRANT SELECT ON {}.call_teardown_outbox TO {}", fixture.target, fixture.runtime),
    ] {
        sqlx::query(&statement).execute(&mut *replacement).await.expect("prepare replacement schema");
    }
    replacement
        .commit()
        .await
        .expect("commit schema replacement");
    assert!(matches!(
        owner_task.await.expect("join owner identity race"),
        Err(CallTeardownOutboxError::UnsafeOwnerBoundary)
    ));
    assert!(matches!(
        runtime_error(
            attach_task
                .await
                .expect("join schema race")
                .err()
                .expect("reject schema replacement")
        ),
        CallTeardownRuntimeAttachError::Database(_)
    ));
    let mut restore = fixture.admin.begin().await.expect("restore schema");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *restore)
        .await
        .expect("hold catalog-X for schema restore");
    sqlx::query(&format!("DROP SCHEMA {} CASCADE", fixture.target))
        .execute(&mut *restore)
        .await
        .expect("drop replacement schema");
    sqlx::query(&format!(
        "ALTER SCHEMA {old_schema} RENAME TO {}",
        fixture.target
    ))
    .execute(&mut *restore)
    .await
    .expect("restore original schema");
    restore
        .commit()
        .await
        .expect("commit original schema restore");
    CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
        .await
        .expect("restored identity attaches");

    drop(attached);
    drop(owner_db);
    drop(runtime);
    drop(pool);
    run_admin(&fixture, &format!("DROP SCHEMA {} CASCADE", fixture.target)).await;
    run_admin(
        &fixture,
        &format!("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {owner}"),
    )
    .await;
    run_admin(&fixture, &format!("REVOKE {owner} FROM {migrator}")).await;
    run_admin(&fixture, &format!("DROP OWNED BY {owner}")).await;
    run_admin(&fixture, &format!("DROP OWNED BY {migrator}")).await;
    run_admin(&fixture, &format!("DROP ROLE {migrator}")).await;
    run_admin(&fixture, &format!("DROP ROLE {owner}")).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn call_teardown_attachment_rejects_non_postgres_and_unattested_handles() {
    let node_identity = node_identity();
    let sqlite = Database::in_memory("call teardown attach SQLite")
        .await
        .expect("SQLite test database");
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(sqlite.clone(), node_identity.clone())
                .await
                .err()
                .expect("SQLite attach is unsupported")
        ),
        CallTeardownRuntimeAttachError::AttestedPostgresRequired
    ));
    assert!(matches!(
        CallTeardownOutboxStore::try_new(sqlite.clone()).await,
        Ok(_)
    ));

    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let raw = Database::from_config(
        "unattested call teardown",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &fixture.target),
        ),
    )
    .await
    .expect("raw PostgreSQL handle");
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(raw, node_identity)
                .await
                .err()
                .expect("unattested PostgreSQL attach is unsupported")
        ),
        CallTeardownRuntimeAttachError::AttestedPostgresRequired
    ));
    fixture.cleanup().await;
}
