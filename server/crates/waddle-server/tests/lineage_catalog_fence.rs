//! PostgreSQL witnesses for the lineage mutators' catalog fence.
//!
//! Each test uses its own disposable PostgreSQL 17 database because advisory
//! locks are scoped to a database, not a schema. PostgreSQL coverage is
//! optional in local development and skips unless WADDLE_TEST_POSTGRES_URL is
//! configured.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::PgConnection;
use sqlx::{Connection, PgPool, Row};
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{self, AdoptOutcome, DeploymentUuid, LineageUuid};
use waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};

// Kept in sync with lineage::LINEAGE_ADVISORY_LOCK_KEY, which is deliberately
// crate-private. The test observes the public operation's PostgreSQL lock.
const LINEAGE_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_993;

struct Fixture {
    cluster: PgPool,
    admin: PgPool,
    database: String,
    database_url: String,
    application: String,
    db: Database,
}

impl Fixture {
    async fn new(label: &str) -> Option<Self> {
        let Some(cluster_url) = waddle_ws_test_support::require_test_postgres_url() else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set ({label})");
            return None;
        };
        let cluster = PgPool::connect(&cluster_url)
            .await
            .expect("connect PostgreSQL cluster");
        let database = format!(
            "waddle_lineage_fence_{}_{}",
            label,
            uuid::Uuid::new_v4().simple()
        );
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&cluster)
            .await
            .expect("create isolated PostgreSQL database");

        let mut parsed_url = url::Url::parse(&cluster_url).expect("parse PostgreSQL URL");
        parsed_url.set_path(&format!("/{database}"));
        let database_url = parsed_url.to_string();
        let admin = PgPool::connect(&database_url)
            .await
            .expect("connect isolated PostgreSQL database");
        let version: i32 = sqlx::query_scalar(
            "SELECT pg_catalog.current_setting('server_version_num')::pg_catalog.int4",
        )
        .fetch_one(&admin)
        .await
        .expect("read PostgreSQL version");
        assert!((170_000..180_000).contains(&version), "PG17 is required");

        let application = format!("lineage_fence_{}", uuid::Uuid::new_v4().simple());
        let db = open_database(&database_url, &application, None).await;
        Some(Self {
            cluster,
            admin,
            database,
            database_url,
            application,
            db,
        })
    }

    async fn cleanup(self) {
        let Self {
            cluster,
            admin,
            database,
            application,
            db,
            ..
        } = self;
        drop(db);
        admin.close().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let gone: bool = sqlx::query_scalar(
                    "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                     WHERE datname = $1 AND application_name = $2)",
                )
                .bind(&database)
                .bind(&application)
                .fetch_one(&cluster)
                .await
                .expect("inspect lineage test pool shutdown");
                if gone {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("lineage pool closes before database cleanup");
        sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
            .execute(&cluster)
            .await
            .expect("drop isolated PostgreSQL database");
        cluster.close().await;
    }
}

async fn open_database(database_url: &str, application: &str, schema: Option<&str>) -> Database {
    let mut parsed_url = url::Url::parse(database_url).expect("parse PostgreSQL URL");
    let retained: Vec<(String, String)> = parsed_url
        .query_pairs()
        .filter(|(key, _)| key != "application_name" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    parsed_url
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("application_name", application);
    if let Some(schema) = schema {
        parsed_url
            .query_pairs_mut()
            .append_pair("options", &format!("-c search_path={schema}"));
    }
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, parsed_url.to_string());
    config.pool_size = 1;
    Database::from_config("lineage catalog-fence test", &config)
        .await
        .expect("open one-connection PostgreSQL pool")
}

fn configured() -> LineageConfig {
    LineageConfig {
        deployment_uuid: Some(
            DeploymentUuid::from_str(&uuid::Uuid::new_v4().to_string())
                .expect("valid deployment UUID"),
        ),
        action: None,
    }
}

async fn hold_shared_catalog_fence(database_url: &str, admin: &PgPool) -> (PgConnection, i32) {
    let mut connection = PgConnection::connect(database_url)
        .await
        .expect("connect shared catalog-fence holder");
    let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut connection)
        .await
        .expect("read shared-fence holder PID");
    let locked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock_shared($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(&mut connection)
        .await
        .expect("hold shared catalog fence");
    assert!(locked, "test database has no other catalog-fence holder");
    assert_advisory_lock(admin, pid, CATALOG_MUTATION_ADVISORY_LOCK_KEY, "ShareLock").await;
    (connection, pid)
}

async fn release_shared_catalog_fence(connection: &mut PgConnection) {
    let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock_shared($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(&mut *connection)
        .await
        .expect("release shared catalog fence");
    assert!(unlocked);
}

async fn assert_advisory_lock(observer: &PgPool, pid: i32, key: i64, mode: &str) {
    let held: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
         WHERE pid = $1 AND locktype = 'advisory' AND granted AND mode = $3 \
           AND objsubid = 1 AND ((classid::pg_catalog.int8 << 32) | objid::pg_catalog.int8) = $2)",
    )
    .bind(pid)
    .bind(key)
    .bind(mode)
    .fetch_one(observer)
    .await
    .expect("inspect PostgreSQL advisory lock");
    assert!(
        held,
        "backend {pid} does not hold {mode} advisory lock {key}"
    );
}

async fn wait_for_idle_rollback(admin: &PgPool, application: &str) -> i32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT a.pid FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.state = 'idle' AND a.query = 'ROLLBACK' \
                   AND a.xact_start IS NULL AND a.backend_xmin IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
                     WHERE l.pid = a.pid AND l.locktype = 'advisory')",
            )
            .bind(application)
            .fetch_optional(admin)
            .await
            .expect("inspect lineage retry rollback");
            if let Some(pid) = pid {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("lineage retry acknowledges rollback and releases locks");
}

async fn assert_clean_retry_window(admin: &PgPool, application: &str) -> i32 {
    let first_pid = wait_for_idle_rollback(admin, application).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut samples = 0;
        while samples < 5 {
            let row = sqlx::query(
                "SELECT a.pid, a.xact_start IS NULL AS no_xact, \
                        a.backend_xmin IS NULL AS no_xmin, \
                        NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
                          WHERE l.pid = a.pid AND l.locktype = 'advisory') AS no_advisory \
                 FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.state = 'idle' AND a.query = 'ROLLBACK'",
            )
            .bind(application)
            .fetch_optional(admin)
            .await
            .expect("sample lineage retry state");
            if let Some(row) = row {
                let pid: i32 = row.try_get("pid").expect("retry backend PID");
                let no_xact: bool = row.try_get("no_xact").expect("retry transaction state");
                let no_xmin: bool = row.try_get("no_xmin").expect("retry xmin state");
                let no_advisory: bool = row
                    .try_get("no_advisory")
                    .expect("retry advisory lock state");
                assert_eq!(pid, first_pid, "retries stay on the same pooled backend");
                assert!(no_xact, "retry backend has no active transaction");
                assert!(no_xmin, "retry backend retains no active xmin");
                assert!(no_advisory, "retry backend retains no advisory lock");
                samples += 1;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("observe several clean rollback boundaries on one backend");
    first_pid
}

async fn lineage_row(admin: &PgPool) -> Option<String> {
    sqlx::query_scalar("SELECT lineage_uuid FROM public._lineage WHERE id = 1")
        .fetch_optional(admin)
        .await
        .expect("read lineage row")
}

async fn assert_lineage_waiter_has_both_fences(admin: &PgPool, application: &str) -> i32 {
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT a.pid FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.wait_event_type = 'Lock' \
                   AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
                     WHERE l.pid = a.pid AND l.locktype = 'relation' \
                       AND l.relation = pg_catalog.to_regclass('public._lineage') \
                       AND NOT l.granted)",
            )
            .bind(application)
            .fetch_optional(admin)
            .await
            .expect("inspect relation-blocked lineage operation");
            if let Some(pid) = pid {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("lineage operation reaches the locked table after taking fences");
    assert_advisory_lock(
        admin,
        pid,
        CATALOG_MUTATION_ADVISORY_LOCK_KEY,
        "ExclusiveLock",
    )
    .await;
    assert_advisory_lock(admin, pid, LINEAGE_ADVISORY_LOCK_KEY, "ExclusiveLock").await;
    pid
}

#[tokio::test]
async fn postgres_lineage_mutators_obey_shared_catalog_fence() {
    let Some(fixture) = Fixture::new("shared").await else {
        return;
    };
    let config = configured();

    // ensure_table cannot create _lineage until the shared catalog holder exits.
    let (mut shared, _) = hold_shared_catalog_fence(&fixture.database_url, &fixture.admin).await;
    let db = fixture.db.clone();
    let ensure = tokio::spawn(async move { lineage::ensure_table(&db).await });
    let ensure_pid = assert_clean_retry_window(&fixture.admin, &fixture.application).await;
    let table_exists: bool =
        sqlx::query_scalar("SELECT pg_catalog.to_regclass('public._lineage') IS NOT NULL")
            .fetch_one(&fixture.admin)
            .await
            .expect("inspect lineage table while catalog fence is held");
    assert!(
        !table_exists,
        "ensure_table did not perform DDL before catalog-X"
    );
    release_shared_catalog_fence(&mut shared).await;
    ensure
        .await
        .expect("ensure task")
        .expect("ensure table after release");
    let live_pid: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = $1",
    )
    .bind(&fixture.application)
    .fetch_one(&fixture.admin)
    .await
    .expect("read lineage operation PID");
    assert_eq!(
        live_pid, ensure_pid,
        "retry kept the same backend through commit"
    );
    shared.close().await.expect("close shared catalog holder");

    // Enrollment's committed row remains absent while catalog-X is blocked.
    let (mut shared, _) = hold_shared_catalog_fence(&fixture.database_url, &fixture.admin).await;
    let db = fixture.db.clone();
    let config_for_task = config.clone();
    let enroll = tokio::spawn(async move { lineage::enroll(&db, &config_for_task).await });
    assert_clean_retry_window(&fixture.admin, &fixture.application).await;
    assert_eq!(lineage_row(&fixture.admin).await, None);
    release_shared_catalog_fence(&mut shared).await;
    let enrolled = enroll
        .await
        .expect("enrollment task")
        .expect("enroll after catalog release");
    shared.close().await.expect("close shared catalog holder");
    assert_eq!(
        lineage_row(&fixture.admin).await,
        Some(enrolled.lineage_uuid.to_string())
    );

    // Adoption has no visible effect until it owns catalog-X too.
    let (mut shared, _) = hold_shared_catalog_fence(&fixture.database_url, &fixture.admin).await;
    let db = fixture.db.clone();
    let config_for_task = config.clone();
    let expected = enrolled.lineage_uuid;
    let adopt =
        tokio::spawn(
            async move { lineage::adopt_if_matched(&db, &config_for_task, &[expected]).await },
        );
    assert_clean_retry_window(&fixture.admin, &fixture.application).await;
    assert_eq!(
        lineage_row(&fixture.admin).await,
        Some(enrolled.lineage_uuid.to_string())
    );
    release_shared_catalog_fence(&mut shared).await;
    let AdoptOutcome::Adopted { matched, attested } = adopt
        .await
        .expect("adoption task")
        .expect("adopt after catalog release")
    else {
        panic!("expected the matching lineage to be adopted");
    };
    shared.close().await.expect("close shared catalog holder");
    assert_eq!(matched, enrolled.lineage_uuid);
    assert_ne!(attested.lineage_uuid, enrolled.lineage_uuid);
    assert_eq!(
        lineage_row(&fixture.admin).await,
        Some(attested.lineage_uuid.to_string())
    );

    // A relation-lock wait gives a stable catalog view of both held locks.
    let mut relation_blocker = PgConnection::connect(&fixture.database_url)
        .await
        .expect("connect lineage relation blocker");
    sqlx::query("BEGIN")
        .execute(&mut relation_blocker)
        .await
        .expect("begin relation blocker");
    sqlx::query("LOCK TABLE public._lineage IN ACCESS EXCLUSIVE MODE")
        .execute(&mut relation_blocker)
        .await
        .expect("hold lineage relation lock");
    let db = fixture.db.clone();
    let config_for_task = config.clone();
    let stable_uuid = attested.lineage_uuid;
    let verify = tokio::spawn(async move { lineage::enroll(&db, &config_for_task).await });
    assert_lineage_waiter_has_both_fences(&fixture.admin, &fixture.application).await;
    sqlx::query("ROLLBACK")
        .execute(&mut relation_blocker)
        .await
        .expect("release lineage relation lock");
    let verified = verify
        .await
        .expect("verification task")
        .expect("verify after relation lock release");
    assert_eq!(verified.lineage_uuid, stable_uuid);

    relation_blocker
        .close()
        .await
        .expect("close relation blocker");
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_lineage_retry_releases_catalog_and_serializes_races() {
    let Some(fixture) = Fixture::new("retry").await else {
        return;
    };
    let config = configured();
    lineage::ensure_table(&fixture.db)
        .await
        .expect("create lineage table");
    let enrolled = lineage::enroll(&fixture.db, &config)
        .await
        .expect("enroll initial row");

    // A lineage-only session blocker forces transaction rollback. At each
    // observed retry boundary, pg_stat_activity has no xact/xmin and pg_locks
    // has no advisory lock for the one-connection application backend.
    let mut lineage_blocker = PgConnection::connect(&fixture.database_url)
        .await
        .expect("connect lineage-only blocker");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut lineage_blocker)
        .await
        .expect("read lineage blocker PID");
    let locked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock($1)")
        .bind(LINEAGE_ADVISORY_LOCK_KEY)
        .fetch_one(&mut lineage_blocker)
        .await
        .expect("hold lineage lock only");
    assert!(locked);
    assert_advisory_lock(
        &fixture.admin,
        blocker_pid,
        LINEAGE_ADVISORY_LOCK_KEY,
        "ExclusiveLock",
    )
    .await;

    let db = fixture.db.clone();
    let config_for_task = config.clone();
    let expected = enrolled.lineage_uuid;
    let adoption =
        tokio::spawn(
            async move { lineage::adopt_if_matched(&db, &config_for_task, &[expected]).await },
        );
    let retry_pid = assert_clean_retry_window(&fixture.admin, &fixture.application).await;
    assert_eq!(
        lineage_row(&fixture.admin).await,
        Some(enrolled.lineage_uuid.to_string())
    );

    let mut catalog_probe = PgConnection::connect(&fixture.database_url)
        .await
        .expect("connect catalog-fence probe");
    let shared_available: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock_shared($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&mut catalog_probe)
            .await
            .expect("probe catalog fence between attempts");
    assert!(
        shared_available,
        "failed lineage attempt rolled back catalog-X"
    );
    let shared_released: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock_shared($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&mut catalog_probe)
            .await
            .expect("release catalog probe");
    assert!(shared_released);

    let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
        .bind(LINEAGE_ADVISORY_LOCK_KEY)
        .fetch_one(&mut lineage_blocker)
        .await
        .expect("release lineage-only blocker");
    assert!(unlocked);
    let AdoptOutcome::Adopted { matched, attested } =
        tokio::time::timeout(Duration::from_secs(5), adoption)
            .await
            .expect("lineage retry progresses after blocker release")
            .expect("adoption task")
            .expect("adoption succeeds after retry")
    else {
        panic!("expected lineage adoption after retry");
    };
    assert_eq!(matched, enrolled.lineage_uuid);
    assert_ne!(attested.lineage_uuid, enrolled.lineage_uuid);
    assert_eq!(
        lineage_row(&fixture.admin).await,
        Some(attested.lineage_uuid.to_string())
    );
    assert!(retry_pid > 0);
    lineage_blocker
        .close()
        .await
        .expect("close lineage blocker");
    catalog_probe.close().await.expect("close catalog probe");

    // Two independent pools race the same first enrollment. Both calls must
    // observe the single committed winner rather than a duplicate-row error.
    let schema = format!("lineage_race_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&fixture.admin)
        .await
        .expect("create concurrent enrollment schema");
    let enroll_a_name = format!("{}_enroll_a", fixture.application);
    let enroll_b_name = format!("{}_enroll_b", fixture.application);
    let enroll_a = open_database(&fixture.database_url, &enroll_a_name, Some(&schema)).await;
    let enroll_b = open_database(&fixture.database_url, &enroll_b_name, Some(&schema)).await;
    let (first, second) = tokio::join!(
        lineage::enroll(&enroll_a, &config),
        lineage::enroll(&enroll_b, &config)
    );
    let first = first.expect("first concurrent enrollment");
    let second = second.expect("second concurrent enrollment");
    assert_eq!(first.lineage_uuid, second.lineage_uuid);
    let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {schema}._lineage"))
        .fetch_one(&fixture.admin)
        .await
        .expect("count concurrent enrollment winners");
    assert_eq!(rows, 1);
    drop(enroll_a);
    drop(enroll_b);

    // Concurrent adoption of the same UUID has exactly one winner; the other
    // caller sees the rotated row and returns NotMatched.
    let second_application = format!("{}_adopt_b", fixture.application);
    let second_db = open_database(&fixture.database_url, &second_application, None).await;
    let current_uuid: LineageUuid = attested.lineage_uuid;
    let db = fixture.db.clone();
    let config_a = config.clone();
    let config_b = config.clone();
    let (first, second) = tokio::join!(
        lineage::adopt_if_matched(&db, &config_a, &[current_uuid]),
        lineage::adopt_if_matched(&second_db, &config_b, &[current_uuid])
    );
    let first = first.expect("first concurrent adoption");
    let second = second.expect("second concurrent adoption");
    let winners: Vec<_> = [&first, &second]
        .into_iter()
        .filter_map(|outcome| match outcome {
            AdoptOutcome::Adopted { attested, .. } => Some(attested.lineage_uuid),
            AdoptOutcome::NotMatched => None,
        })
        .collect();
    let not_matched = [&first, &second]
        .into_iter()
        .filter(|outcome| matches!(outcome, AdoptOutcome::NotMatched))
        .count();
    assert_eq!(winners.len(), 1);
    assert_eq!(not_matched, 1);
    assert_ne!(winners[0], current_uuid);
    assert_eq!(
        lineage_row(&fixture.admin).await,
        Some(winners[0].to_string())
    );
    drop(second_db);

    fixture.cleanup().await;
}
