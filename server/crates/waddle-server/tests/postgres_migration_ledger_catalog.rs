//! PostgreSQL witnesses for the ordinary migration runner's ledger preflight.

use std::time::Duration;

use sqlx::PgPool;
use waddle_server::db::{
    Database, DatabaseConfig, DatabaseDriver, DatabaseError, MigrationLedgerError, MigrationRunner,
};

struct Fixture {
    admin: PgPool,
    db: Database,
    schema: String,
}

impl Fixture {
    async fn new(url: &str, prefix: &str) -> Self {
        let schema = format!("ledger_{prefix}_{}", uuid::Uuid::new_v4().simple());
        let admin = PgPool::connect(url)
            .await
            .expect("connect test administrator");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let db = open_database(url, &schema, "ledger_preflight_test").await;
        Self { admin, db, schema }
    }

    async fn execute(&self, sql: &str) {
        sqlx::raw_sql(sql)
            .execute(&self.admin)
            .await
            .expect("execute fixture SQL");
    }

    async fn probe_called(&self) -> bool {
        sqlx::query_scalar(&format!("SELECT is_called FROM {}.probe", self.schema))
            .fetch_one(&self.admin)
            .await
            .expect("read sequence probe")
    }

    async fn reset_probe(&self) {
        sqlx::query(&format!(
            "SELECT pg_catalog.setval('{}.probe'::pg_catalog.regclass, 1, false)",
            self.schema
        ))
        .execute(&self.admin)
        .await
        .expect("reset sequence probe");
    }

    async fn close(self) {
        drop(self.db);
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop isolated schema");
        self.admin.close().await;
    }
}

async fn open_database(url: &str, search_path: &str, application: &str) -> Database {
    let mut scoped = url::Url::parse(url).expect("parse test URL");
    scoped
        .query_pairs_mut()
        .append_pair("options", &format!("-c search_path={search_path}"))
        .append_pair("application_name", application);
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, scoped.to_string());
    // In particular, the TEMP-shadow witness must return to the same backend.
    config.pool_size = 1;
    Database::from_config("ledger-preflight-test", &config)
        .await
        .expect("open test database")
}

fn ordinary_table(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}._migrations (version BIGINT PRIMARY KEY, \
         description TEXT NOT NULL, checksum TEXT, \
         applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP)"
    )
}

fn assert_unsafe_catalog(result: Result<Vec<i64>, DatabaseError>) {
    assert!(
        matches!(
            result,
            Err(DatabaseError::MigrationLedger(
                MigrationLedgerError::UnsafeCatalog
            ))
        ),
        "unexpected runner result: {result:?}"
    );
}

#[tokio::test]
async fn postgres_clean_legacy_and_foreign_ledger_rows() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, "clean").await;
    let runner = MigrationRunner::single();
    assert!(!runner
        .run(&fixture.db)
        .await
        .expect("initial migrations")
        .is_empty());
    assert!(runner
        .run(&fixture.db)
        .await
        .expect("idempotent run")
        .is_empty());

    fixture
        .execute(&format!(
            "ALTER TABLE {}._migrations DROP COLUMN checksum",
            fixture.schema
        ))
        .await;
    assert!(runner
        .run(&fixture.db)
        .await
        .expect("legacy backfill")
        .is_empty());
    let missing: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}._migrations WHERE checksum IS NULL",
        fixture.schema
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect backfilled checksums");
    assert_eq!(missing, 0);

    // A runner owns every version in the ledger, so one whose catalog lacks
    // the baseline refuses the baseline row rather than ignoring it.
    let foreign = MigrationRunner::new(Vec::new());
    let refused = foreign.run(&fixture.db).await;
    assert!(
        matches!(
            refused,
            Err(DatabaseError::MigrationLedger(
                MigrationLedgerError::UnknownVersion { version: 2000, .. }
            ))
        ),
        "unexpected runner result: {refused:?}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn postgres_hostile_shapes_are_refused_before_side_effects() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, "hostile").await;
    let s = &fixture.schema;
    fixture
        .execute(&format!(
            "CREATE SEQUENCE {s}.probe; \
             CREATE FUNCTION {s}.bump() RETURNS bigint LANGUAGE plpgsql IMMUTABLE AS $$ \
             BEGIN PERFORM pg_catalog.nextval('{s}.probe'::pg_catalog.regclass); RETURN 1; END $$; \
             CREATE FUNCTION {s}.bump_time() RETURNS timestamptz LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM pg_catalog.nextval('{s}.probe'::pg_catalog.regclass); RETURN CURRENT_TIMESTAMP; END $$; \
             CREATE FUNCTION {s}.bump_time_arg(text) RETURNS timestamptz LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM pg_catalog.nextval('{s}.probe'::pg_catalog.regclass); RETURN CURRENT_TIMESTAMP; END $$; \
             CREATE FUNCTION {s}.bump_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM pg_catalog.nextval('{s}.probe'::pg_catalog.regclass); RETURN NEW; END $$"
        ))
        .await;
    let runner = MigrationRunner::new(Vec::new());

    // Positive control proves that reading this view runs the probe.
    fixture
        .execute(&format!(
            "CREATE VIEW {s}._migrations AS SELECT {s}.bump() AS version, \
             'view'::text AS description, NULL::text AS checksum, CURRENT_TIMESTAMP AS applied_at"
        ))
        .await;
    let _: i64 = sqlx::query_scalar(&format!("SELECT version FROM {s}._migrations"))
        .fetch_one(&fixture.admin)
        .await
        .expect("view positive control");
    assert!(fixture.probe_called().await);
    fixture.reset_probe().await;
    assert_unsafe_catalog(runner.run(&fixture.db).await);
    assert!(!fixture.probe_called().await);
    fixture.execute(&format!("DROP VIEW {s}._migrations")).await;

    fixture.execute(&ordinary_table(s)).await;
    fixture
        .execute(&format!(
            "CREATE TRIGGER hostile BEFORE INSERT OR UPDATE ON {s}._migrations \
             FOR EACH ROW EXECUTE FUNCTION {s}.bump_trigger(); \
             INSERT INTO {s}._migrations(version, description) VALUES (1, 'control'); \
             TRUNCATE {s}._migrations"
        ))
        .await;
    assert!(fixture.probe_called().await);
    fixture.reset_probe().await;
    assert_unsafe_catalog(runner.run(&fixture.db).await);
    assert!(!fixture.probe_called().await);
    fixture
        .execute(&format!("DROP TABLE {s}._migrations"))
        .await;

    fixture
        .execute(&format!(
            "CREATE TABLE {s}._migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, \
             checksum TEXT, applied_at TIMESTAMPTZ NOT NULL DEFAULT {s}.bump_time()); \
             INSERT INTO {s}._migrations(version, description) VALUES (1, 'control'); \
             TRUNCATE {s}._migrations"
        ))
        .await;
    assert!(fixture.probe_called().await);
    fixture.reset_probe().await;
    assert_unsafe_catalog(runner.run(&fixture.db).await);
    assert!(!fixture.probe_called().await);
    fixture
        .execute(&format!("DROP TABLE {s}._migrations"))
        .await;

    let large_default = "x".repeat(32_000);
    fixture
        .execute(&format!(
            "CREATE TABLE {s}._migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, \
             checksum TEXT, applied_at TIMESTAMPTZ NOT NULL DEFAULT {s}.bump_time_arg('{large_default}')); \
             INSERT INTO {s}._migrations(version, description) VALUES (1, 'control'); \
             TRUNCATE {s}._migrations"
        ))
        .await;
    assert!(fixture.probe_called().await);
    fixture.reset_probe().await;
    let (stored_bytes, compression): (i32, Option<String>) = sqlx::query_as(
        "SELECT pg_catalog.pg_column_size(d.adbin), pg_catalog.pg_column_compression(d.adbin) \
         FROM pg_catalog.pg_attrdef d WHERE d.adrelid = pg_catalog.to_regclass($1)",
    )
    .bind(format!("{s}._migrations"))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect oversized expression storage");
    assert!(stored_bytes > 256 || compression.is_some());
    assert_unsafe_catalog(runner.run(&fixture.db).await);
    assert!(!fixture.probe_called().await);
    fixture
        .execute(&format!("DROP TABLE {s}._migrations"))
        .await;

    fixture
        .execute(&format!(
            "CREATE DOMAIN {s}.hostile AS BIGINT CHECK ({s}.bump() > 0); \
             CREATE TABLE {s}._migrations (version {s}.hostile PRIMARY KEY, description TEXT NOT NULL, \
             checksum TEXT, applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP)"
        ))
        .await;
    let _: i64 = sqlx::query_scalar(&format!("SELECT 1::{s}.hostile::bigint"))
        .fetch_one(&fixture.admin)
        .await
        .expect("domain positive control");
    assert!(fixture.probe_called().await);
    fixture.reset_probe().await;
    assert_unsafe_catalog(runner.run(&fixture.db).await);
    assert!(!fixture.probe_called().await);
    fixture
        .execute(&format!("DROP TABLE {s}._migrations"))
        .await;

    fixture.execute(&ordinary_table(s)).await;
    fixture
        .execute(&format!(
            "CREATE INDEX hostile ON {s}._migrations (({s}.bump()))"
        ))
        .await;
    let _: i64 = sqlx::query_scalar(&format!("SELECT {s}.bump()"))
        .fetch_one(&fixture.admin)
        .await
        .expect("index function positive control");
    assert!(fixture.probe_called().await);
    fixture.reset_probe().await;
    assert_unsafe_catalog(runner.run(&fixture.db).await);
    assert!(!fixture.probe_called().await);
    fixture
        .execute(&format!("DROP TABLE {s}._migrations"))
        .await;

    for alteration in [
        "ALTER TABLE {table} ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE {table} SET UNLOGGED",
        "ALTER TABLE {table} ADD CONSTRAINT extra CHECK (version > 0)",
        "CREATE RULE extra AS ON UPDATE TO {table} DO INSTEAD NOTHING",
    ] {
        fixture.execute(&ordinary_table(s)).await;
        fixture
            .execute(&alteration.replace("{table}", &format!("{s}._migrations")))
            .await;
        assert_unsafe_catalog(runner.run(&fixture.db).await);
        fixture
            .execute(&format!("DROP TABLE {s}._migrations"))
            .await;
    }
    fixture.close().await;
}

#[tokio::test]
async fn postgres_inherited_child_is_rejected_without_waiting_for_its_lock() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, "inherit").await;
    fixture.execute(&ordinary_table(&fixture.schema)).await;
    fixture
        .execute(&format!(
            "CREATE TABLE {}.child () INHERITS ({}._migrations)",
            fixture.schema, fixture.schema
        ))
        .await;
    let mut child_lock = fixture.admin.begin().await.expect("start child lock");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.child IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *child_lock)
    .await
    .expect("lock child");
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        MigrationRunner::new(Vec::new()).run(&fixture.db),
    )
    .await
    .expect("preflight must not recursively wait for child");
    assert_unsafe_catalog(result);
    child_lock.rollback().await.expect("release child lock");
    fixture.close().await;
}

#[tokio::test]
async fn postgres_temp_and_alternate_schema_shadows_are_not_used() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, "shadow").await;
    let other = format!("{}_other", fixture.schema);
    fixture
        .execute(&format!(
            "CREATE SCHEMA {other}; CREATE SEQUENCE {other}.probe; \
             CREATE FUNCTION {other}.bump() RETURNS bigint LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM pg_catalog.nextval('{other}.probe'::pg_catalog.regclass); RETURN 1; END $$; \
             CREATE VIEW {other}._migrations AS SELECT {other}.bump() AS version"
        ))
        .await;
    let db = open_database(
        &url,
        &format!("{},{other}", fixture.schema),
        "ledger_shadow",
    )
    .await;
    let guard = db.guard().await.expect("obtain sole connection");
    guard
        .execute(
            &format!("CREATE TEMP VIEW _migrations AS SELECT {other}.bump() AS version"),
            (),
        )
        .await
        .expect("install temporary shadow");
    guard
        .execute("SELECT version FROM _migrations", ())
        .await
        .expect("temporary view positive control");
    drop(guard);
    let called: bool = sqlx::query_scalar(&format!("SELECT is_called FROM {other}.probe"))
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect positive control");
    assert!(called);
    sqlx::query(&format!(
        "SELECT pg_catalog.setval('{other}.probe'::pg_catalog.regclass, 1, false)"
    ))
    .execute(&fixture.admin)
    .await
    .expect("reset shadow probe");
    MigrationRunner::new(Vec::new())
        .run(&db)
        .await
        .expect("create qualified ledger in intended schema");
    let target_exists: bool = sqlx::query_scalar(&format!(
        "SELECT pg_catalog.to_regclass('{}._migrations') IS NOT NULL",
        fixture.schema
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect intended ledger");
    assert!(target_exists);
    let called: bool = sqlx::query_scalar(&format!("SELECT is_called FROM {other}.probe"))
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect alternate schema probe");
    assert!(!called);
    drop(db);
    fixture
        .execute(&format!("DROP SCHEMA {other} CASCADE"))
        .await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_leading_schema_cannot_shadow_catalog_text_during_target_discovery() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, "type_shadow").await;
    let s = &fixture.schema;
    fixture
        .execute(&format!(
            "CREATE SEQUENCE {s}.probe; \
             CREATE FUNCTION {s}.check_name(pg_catalog.name) RETURNS pg_catalog.bool \
             LANGUAGE plpgsql AS $$ BEGIN \
               PERFORM pg_catalog.nextval('{s}.probe'::pg_catalog.regclass); \
               PERFORM pg_catalog.pg_sleep(0.25); RETURN true; END $$; \
             CREATE DOMAIN {s}.text AS pg_catalog.name CHECK ({s}.check_name(VALUE))"
        ))
        .await;

    // The former unqualified cast resolves to the hostile domain and times
    // out after advancing the nontransactional sequence. This establishes
    // that the witness detects both user code and an obstructed discovery.
    let mut control = fixture.admin.begin().await.expect("start positive control");
    sqlx::query(&format!("SET LOCAL search_path = {s}, pg_catalog"))
        .execute(&mut *control)
        .await
        .expect("install hostile search path");
    sqlx::query("SET LOCAL statement_timeout = '50ms'")
        .execute(&mut *control)
        .await
        .expect("bound positive control");
    let error = sqlx::query("SELECT pg_catalog.current_schema()::text")
        .execute(&mut *control)
        .await
        .expect_err("unqualified cast must invoke the hostile domain");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("57014")
    );
    control
        .rollback()
        .await
        .expect("roll back positive control");
    assert!(fixture.probe_called().await);
    fixture.reset_probe().await;

    let mut timed_url = url::Url::parse(&url).expect("parse test URL");
    timed_url
        .query_pairs_mut()
        .append_pair("options", "-c statement_timeout=2s");
    let db = open_database(
        timed_url.as_str(),
        &format!("{s},pg_catalog"),
        "ledger_type_shadow",
    )
    .await;
    tokio::time::timeout(
        Duration::from_secs(3),
        MigrationRunner::new(Vec::new()).run(&db),
    )
    .await
    .expect("qualified discovery must not wait for the hostile domain")
    .expect("qualified discovery and ledger creation");
    assert!(!fixture.probe_called().await);
    drop(db);
    fixture.close().await;
}

#[tokio::test]
async fn postgres_replaced_name_after_lock_wait_is_refused() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, "wait").await;
    let s = &fixture.schema;
    fixture.execute(&ordinary_table(s)).await;
    fixture
        .execute(&format!(
            "CREATE SEQUENCE {s}.probe; \
             CREATE FUNCTION {s}.bump() RETURNS bigint LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM pg_catalog.nextval('{s}.probe'::pg_catalog.regclass); RETURN 1; END $$"
        ))
        .await;
    let application = format!("ledger_wait_{}", uuid::Uuid::new_v4().simple());
    let db = open_database(&url, s, &application).await;
    let mut blocker = fixture.admin.begin().await.expect("start lock holder");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}._migrations IN ACCESS EXCLUSIVE MODE"
    ))
    .execute(&mut *blocker)
    .await
    .expect("hold ledger lock");

    let local = tokio::task::LocalSet::new();
    let mut run = local.spawn_local(async move { MigrationRunner::new(Vec::new()).run(&db).await });
    local
        .run_until(async {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let waiting: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                         WHERE application_name = $1 AND wait_event_type = 'Lock' \
                           AND query LIKE 'LOCK TABLE ONLY %')",
                    )
                    .bind(&application)
                    .fetch_one(&fixture.admin)
                    .await
                    .expect("observe ledger lock wait");
                    if waiting {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("runner must reach relation lock");
        })
        .await;
    sqlx::raw_sql(&format!(
        "DROP TABLE {s}._migrations; \
         CREATE VIEW {s}._migrations AS SELECT {s}.bump() AS version, \
         'view'::text AS description, NULL::text AS checksum, CURRENT_TIMESTAMP AS applied_at"
    ))
    .execute(&mut *blocker)
    .await
    .expect("replace the locked name in the holder transaction");
    blocker.commit().await.expect("release replacement");
    let result = local.run_until(&mut run).await.expect("runner task");
    assert!(result.is_err(), "replacement must not be accepted");
    assert!(!fixture.probe_called().await);
    let _: i64 = sqlx::query_scalar(&format!("SELECT version FROM {s}._migrations"))
        .fetch_one(&fixture.admin)
        .await
        .expect("replacement view positive control");
    assert!(fixture.probe_called().await);
    fixture.close().await;
}

#[tokio::test]
async fn postgres_relation_lock_timeout_rolls_back_and_releases_advisory_locks() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, "timeout").await;
    fixture.execute(&ordinary_table(&fixture.schema)).await;
    let mut blocker = fixture.admin.begin().await.expect("start lock holder");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}._migrations IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *blocker)
    .await
    .expect("hold ledger lock");

    let application = format!("ledger_timeout_{}", uuid::Uuid::new_v4().simple());
    let mut timed_url = url::Url::parse(&url).expect("parse test URL");
    timed_url
        .query_pairs_mut()
        .append_pair("options", "-c lock_timeout=100ms");
    let db = open_database(timed_url.as_str(), &fixture.schema, &application).await;
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        MigrationRunner::new(Vec::new()).run(&db),
    )
    .await
    .expect("tighter lock timeout must be retained");
    assert!(matches!(
        result,
        Err(DatabaseError::MigrationLedger(
            MigrationLedgerError::RelationLockTimeout
        ))
    ));

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let released: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.state = 'idle' AND a.xact_start IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
                     WHERE l.pid = a.pid AND l.locktype = 'advisory'))",
            )
            .bind(&application)
            .fetch_one(&fixture.admin)
            .await
            .expect("inspect runner backend after rollback");
            if released {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("rollback must leave the original backend idle without advisory locks");
    blocker.rollback().await.expect("release lock holder");
    drop(db);
    fixture.close().await;
}

#[tokio::test]
async fn postgres_enabled_event_trigger_is_refused_before_ledger_ddl() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    // Event triggers are database-wide, so use a distinct disposable database
    // rather than interfering with other concurrently running test schemas.
    let cluster = PgPool::connect(&url)
        .await
        .expect("connect test administrator");
    let database = format!("ledger_event_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&cluster)
        .await
        .expect("create disposable database");
    let mut isolated = url::Url::parse(&url).expect("parse test URL");
    isolated.set_path(&format!("/{database}"));
    let fixture = Fixture::new(isolated.as_str(), "event").await;
    let s = &fixture.schema;
    fixture
        .execute(&format!(
            "CREATE SEQUENCE {s}.probe; \
             CREATE FUNCTION {s}.event_probe() RETURNS event_trigger LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM pg_catalog.nextval('{s}.probe'::pg_catalog.regclass); END $$; \
             CREATE EVENT TRIGGER ledger_event_probe ON ddl_command_start \
             EXECUTE FUNCTION {s}.event_probe(); \
             CREATE TABLE {s}.positive_control (id bigint)"
        ))
        .await;
    assert!(fixture.probe_called().await);
    fixture.reset_probe().await;
    let result = MigrationRunner::new(Vec::new()).run(&fixture.db).await;
    assert!(matches!(
        result,
        Err(DatabaseError::MigrationLedger(
            MigrationLedgerError::EnabledEventTrigger
        ))
    ));
    assert!(!fixture.probe_called().await);
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT pg_catalog.to_regclass('{s}._migrations') IS NOT NULL"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("check that ledger DDL did not run");
    assert!(!exists);
    fixture
        .execute("DROP EVENT TRIGGER ledger_event_probe")
        .await;
    fixture.close().await;
    sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .execute(&cluster)
        .await
        .expect("drop disposable database");
    cluster.close().await;
}

#[tokio::test]
async fn sqlite_legacy_checksum_backfill_remains_available() {
    let db = Database::in_memory("ledger-preflight-sqlite")
        .await
        .expect("open SQLite database");
    let runner = MigrationRunner::single();
    assert!(!runner
        .run(&db)
        .await
        .expect("initial migrations")
        .is_empty());
    let guard = db.guard().await.expect("SQLite connection");
    guard
        .execute("ALTER TABLE _migrations DROP COLUMN checksum", ())
        .await
        .expect("restore legacy ledger shape");
    drop(guard);
    assert!(runner.run(&db).await.expect("legacy backfill").is_empty());
}
