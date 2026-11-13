//! The migration runner over the deployment catalog: the schema baseline
//! (version 2000) and the migrations after it. A database the retired
//! migration history built is represented here by a baseline-built schema
//! whose ledger holds the retired manifest, which is what the stamp checks:
//! the round trip in `scripts/schema_baseline.py` proves the two schemas
//! identical.

use super::{sql, BaselineAction, Migration, MigrationRunner, BASELINE_VERSION};
use crate::db::{
    migration_checksum, Database, DatabaseConfig, DatabaseDriver, DatabaseError,
    MigrationLedgerError,
};
use crate::ingress::test_support::{runbook_sql_block, runbook_statements};
use sqlx::{Column, Row};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

const SQLITE_BOOTSTRAP: &str = include_str!("../../../sql/baseline/sqlite.bootstrap.sql");
const SQLITE_RETIRED_LEDGER: &str =
    include_str!("../../../sql/baseline/sqlite.retired-ledger.json");
const POSTGRES_RETIRED_LEDGER: &str =
    include_str!("../../../sql/baseline/postgres.retired-ledger.json");

type LedgerRow = (i64, String, Option<String>);

fn retired_ledger(driver: DatabaseDriver) -> Vec<(i64, String, String)> {
    let text = match driver {
        DatabaseDriver::Sqlite => SQLITE_RETIRED_LEDGER,
        DatabaseDriver::Postgres => POSTGRES_RETIRED_LEDGER,
    };
    serde_json::from_str(text).expect("retired ledger manifest")
}

fn baseline_row(driver: DatabaseDriver) -> LedgerRow {
    let baseline = MigrationRunner::single()
        .migrations
        .into_iter()
        .find(|migration| migration.version == BASELINE_VERSION)
        .expect("the catalog starts with the baseline");
    let checksum = migration_checksum(&baseline, driver);
    (BASELINE_VERSION, baseline.description, Some(checksum))
}

async fn ledger(db: &Database) -> Vec<LedgerRow> {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT version, description, checksum FROM _migrations ORDER BY version",
            (),
        )
        .await
        .expect("read the ledger");
    let mut ledger = Vec::new();
    while let Some(row) = rows.next().await.expect("ledger row") {
        ledger.push((
            row.get(0).expect("version"),
            row.get(1).expect("description"),
            row.get(2).expect("checksum"),
        ));
    }
    ledger
}

async fn replace_ledger(db: &Database, rows: &[(i64, String, String)]) {
    let conn = db.guard().await.expect("database guard");
    conn.execute("DELETE FROM _migrations", ())
        .await
        .expect("clear the ledger");
    for (version, description, checksum) in rows {
        conn.execute(
            "INSERT INTO _migrations (version, description, checksum) VALUES (?, ?, ?)",
            crate::db_params![*version, description.as_str(), checksum.as_str()],
        )
        .await
        .expect("seed a ledger row");
    }
}

async fn sqlite_table_count(db: &Database) -> i64 {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query("SELECT COUNT(*) FROM sqlite_master", ())
        .await
        .expect("count schema objects");
    rows.next()
        .await
        .expect("count row")
        .expect("count exists")
        .get(0)
        .expect("count")
}

/// A SQLite database with the structure the retired history built and a
/// ledger that is exactly the retired manifest. The server creates the
/// lineage and ledger tables before it migrates, so they come first.
async fn sqlite_retired_history_database(name: &str) -> Database {
    let db = Database::in_memory(name).await.expect("open sqlite");
    db.guard()
        .await
        .expect("database guard")
        .execute_batch(SQLITE_BOOTSTRAP)
        .await
        .expect("create the lineage and ledger tables");
    MigrationRunner::deployment(BaselineAction::Apply)
        .run(&db)
        .await
        .expect("build the schema");
    replace_ledger(&db, &retired_ledger(DatabaseDriver::Sqlite)).await;
    db
}

#[tokio::test]
async fn a_fresh_sqlite_database_is_built_from_the_baseline() {
    let db = Database::in_memory("baseline-fresh").await.expect("sqlite");
    let applied = MigrationRunner::single().run(&db).await.expect("migrate");
    assert_eq!(applied, vec![BASELINE_VERSION]);
    assert_eq!(
        ledger(&db).await,
        vec![baseline_row(DatabaseDriver::Sqlite)]
    );
    let conn = db.guard().await.expect("guard");
    for table in [
        "users",
        "channels",
        "ingress_protocol_epoch",
        "sm_durable_sessions",
    ] {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
                crate::db_params![table],
            )
            .await
            .expect("look the table up");
        let found: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("row")
            .get(0)
            .expect("count");
        assert_eq!(found, 1, "the baseline creates {table}");
    }
    let mut rows = conn
        .query("SELECT id, epoch FROM ingress_protocol_epoch", ())
        .await
        .expect("read the epoch");
    let row = rows
        .next()
        .await
        .expect("row")
        .expect("the static epoch row");
    assert_eq!(
        (row.get::<i64>(0).unwrap(), row.get::<i64>(1).unwrap()),
        (1, 0)
    );
}

#[tokio::test]
async fn a_complete_retired_sqlite_ledger_is_stamped_without_ddl() {
    let db = sqlite_retired_history_database("baseline-stamp").await;
    let objects = sqlite_table_count(&db).await;
    let applied = MigrationRunner::deployment(BaselineAction::Stamp)
        .run(&db)
        .await
        .expect("stamp");
    assert!(applied.is_empty(), "a stamp applies nothing");
    assert_eq!(
        ledger(&db).await,
        vec![baseline_row(DatabaseDriver::Sqlite)]
    );
    assert_eq!(sqlite_table_count(&db).await, objects);

    // A second run finds the baseline and changes nothing.
    assert!(MigrationRunner::deployment(BaselineAction::Stamp)
        .run(&db)
        .await
        .expect("second run")
        .is_empty());
    assert_eq!(
        ledger(&db).await,
        vec![baseline_row(DatabaseDriver::Sqlite)]
    );
}

#[tokio::test]
async fn apply_refuses_a_retired_ledger_and_leaves_it() {
    let db = sqlite_retired_history_database("baseline-apply-refuses").await;
    let before = ledger(&db).await;
    let error = MigrationRunner::deployment(BaselineAction::Apply)
        .run(&db)
        .await
        .expect_err("apply never stamps");
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::RetiredLedger { .. })
    ));
    assert_eq!(ledger(&db).await, before);
}

#[tokio::test]
async fn an_older_retired_ledger_is_refused_with_the_release_to_run_first() {
    let db = sqlite_retired_history_database("baseline-stamp-incomplete").await;
    let mut older = retired_ledger(DatabaseDriver::Sqlite);
    let last = older.pop().expect("a last retired version").0;
    replace_ledger(&db, &older).await;
    let before = ledger(&db).await;
    let error = MigrationRunner::single()
        .run(&db)
        .await
        .expect_err("an older release ran last");
    assert!(matches!(
        &error,
        DatabaseError::MigrationLedger(MigrationLedgerError::RetiredLedgerIncomplete {
            missing,
            last: final_version,
        }) if missing == &vec![last] && *final_version == last
    ));
    assert!(error.to_string().contains(&format!("ends at v{last}")));
    assert_eq!(ledger(&db).await, before);
}

#[tokio::test]
async fn a_retired_ledger_with_a_changed_row_or_a_later_version_is_refused() {
    let db = sqlite_retired_history_database("baseline-stamp-mismatch").await;
    let mut changed = retired_ledger(DatabaseDriver::Sqlite);
    changed[0].2 = "0".repeat(64);
    let version = changed[0].0;
    replace_ledger(&db, &changed).await;
    let error = MigrationRunner::single()
        .run(&db)
        .await
        .expect_err("changed");
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::RetiredLedgerMismatch { version: found })
            if found == version
    ));

    let mut mixed = retired_ledger(DatabaseDriver::Sqlite);
    let (_, description, checksum) = baseline_row(DatabaseDriver::Sqlite);
    mixed.push((BASELINE_VERSION, description, checksum.expect("checksum")));
    replace_ledger(&db, &mixed).await;
    let error = MigrationRunner::single().run(&db).await.expect_err("mixed");
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::RetiredLedgerMixed {
            version: BASELINE_VERSION
        })
    ));
}

#[tokio::test]
async fn a_stamp_refuses_a_schema_the_history_did_not_build() {
    let db = sqlite_retired_history_database("baseline-stamp-drift").await;
    db.guard()
        .await
        .expect("guard")
        .execute("CREATE TABLE operator_extra (id INTEGER PRIMARY KEY)", ())
        .await
        .expect("add a table");
    let before = ledger(&db).await;
    let error = MigrationRunner::single().run(&db).await.expect_err("drift");
    assert!(matches!(
        &error,
        DatabaseError::MigrationLedger(MigrationLedgerError::BaselineCatalogMismatch { count: 1, first })
            if first == "unexpected table operator_extra"
    ));
    assert_eq!(ledger(&db).await, before);
}

#[tokio::test]
async fn an_unrecorded_schema_is_refused_before_the_baseline_runs() {
    let db = Database::in_memory("baseline-unrecorded")
        .await
        .expect("sqlite");
    db.guard()
        .await
        .expect("guard")
        .execute("CREATE TABLE users (id TEXT PRIMARY KEY)", ())
        .await
        .expect("create a table the runner did not record");
    let error = MigrationRunner::single()
        .run(&db)
        .await
        .expect_err("unrecorded");
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::SchemaWithoutLedger { ref table })
            if table == "users"
    ));
    assert!(ledger(&db).await.is_empty());
}

#[tokio::test]
async fn a_ledger_version_this_binary_does_not_know_is_refused() {
    let db = Database::in_memory("baseline-unknown")
        .await
        .expect("sqlite");
    MigrationRunner::single().run(&db).await.expect("migrate");
    db.guard()
        .await
        .expect("guard")
        .execute(
            "INSERT INTO _migrations (version, description, checksum) VALUES (?, ?, ?)",
            crate::db_params![BASELINE_VERSION + 1, "from a newer release", "0".repeat(64)],
        )
        .await
        .expect("record a newer migration");
    let error = MigrationRunner::single().run(&db).await.expect_err("newer");
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::UnknownVersion { version, .. })
            if version == BASELINE_VERSION + 1
    ));
}

#[tokio::test]
async fn migrations_after_the_baseline_are_validated_in_order() {
    let migration = |version: i64| Migration {
        version,
        description: format!("migration {version}"),
        sql_sqlite: "SELECT 1;",
        sql_postgres: "SELECT 1;",
    };
    let runner = MigrationRunner::new(vec![
        migration(BASELINE_VERSION),
        migration(BASELINE_VERSION + 1),
        migration(BASELINE_VERSION + 2),
    ]);
    let db = Database::in_memory("baseline-gap").await.expect("sqlite");
    let conn = db.guard().await.expect("guard");
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
        .await
        .expect("ledger table");
    drop(conn);
    let rows: Vec<(i64, String, String)> = [BASELINE_VERSION, BASELINE_VERSION + 2]
        .into_iter()
        .map(|version| {
            let migration = migration(version);
            let checksum = migration_checksum(&migration, DatabaseDriver::Sqlite);
            (version, migration.description, checksum)
        })
        .collect();
    replace_ledger(&db, &rows).await;
    let error = runner.run(&db).await.expect_err("gap");
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::VersionGap { missing, applied_after })
            if missing == BASELINE_VERSION + 1 && applied_after == BASELINE_VERSION + 2
    ));

    let mut changed = rows.clone();
    changed[0].2 = "0".repeat(64);
    replace_ledger(&db, &changed).await;
    let error = runner.run(&db).await.expect_err("checksum");
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::ChecksumMismatch { version, .. })
            if version == BASELINE_VERSION
    ));
}

#[tokio::test]
async fn postgres_fresh_database_is_built_from_the_baseline_in_any_schema() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (baseline on postgres)");
        return;
    };
    let schema = unique_postgres_schema_name("baseline_fresh");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let applied = MigrationRunner::single().run(&db).await.expect("migrate");
    assert_eq!(applied, vec![BASELINE_VERSION]);
    assert_eq!(
        ledger(&db).await,
        vec![baseline_row(DatabaseDriver::Postgres)]
    );
    // The template functions name the schema they were rendered for.
    let attach: String = sqlx::query_scalar(&format!(
        "SELECT prosrc FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n \
           ON n.oid = p.pronamespace WHERE n.nspname = '{schema}' \
           AND p.proname = 'pubsub_runtime_attach_lock'"
    ))
    .fetch_one(&admin)
    .await
    .expect("rendered template function");
    assert!(attach.contains(&format!("\"{schema}\".\"pubsub_items\"")));
    assert!(MigrationRunner::single()
        .run(&db)
        .await
        .expect("rerun")
        .is_empty());
    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_an_older_retired_ledger_is_refused_before_anything_is_written() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (baseline stamp refusal)");
        return;
    };
    let schema = unique_postgres_schema_name("baseline_older");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    MigrationRunner::deployment(BaselineAction::Apply)
        .run(&db)
        .await
        .expect("build the schema");
    let mut older = retired_ledger(DatabaseDriver::Postgres);
    older.pop();
    replace_ledger(&db, &older).await;
    let before = ledger(&db).await;
    let error = MigrationRunner::single().run(&db).await.expect_err("older");
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::RetiredLedgerIncomplete { .. })
    ));
    assert_eq!(ledger(&db).await, before);
    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

#[test]
fn the_retired_ledgers_name_the_same_versions_on_both_engines() {
    let postgres: Vec<i64> = retired_ledger(DatabaseDriver::Postgres)
        .iter()
        .map(|row| row.0)
        .collect();
    let sqlite: Vec<i64> = retired_ledger(DatabaseDriver::Sqlite)
        .iter()
        .map(|row| row.0)
        .collect();
    assert_eq!(postgres, sqlite);
    assert!(postgres.iter().all(|version| *version < BASELINE_VERSION));
}

#[tokio::test]
async fn single_runner_is_idempotent_with_a_stable_ledger() {
    let db = Database::in_memory("idempotent-migration-ledger")
        .await
        .unwrap();
    let runner = MigrationRunner::single();
    let first = runner.run(&db).await.unwrap();
    let count = migration_ledger_row_count(&db).await;
    assert!(!first.is_empty());
    assert!(runner.run(&db).await.unwrap().is_empty());
    assert_eq!(migration_ledger_row_count(&db).await, count);
}

#[tokio::test]
async fn migration_apply_error_names_the_failing_migration_and_preserves_its_source() {
    let db = Database::in_memory("migration-apply-error-context")
        .await
        .expect("open in-memory database");
    let runner = MigrationRunner::new(vec![Migration {
        version: 42,
        description: "invalid migration for error context".to_string(),
        sql_sqlite: "THIS IS NOT VALID SQL;",
        sql_postgres: "THIS IS NOT VALID SQL;",
    }]);

    let error = runner
        .run(&db)
        .await
        .expect_err("invalid migration must fail");
    assert!(matches!(
        &error,
        DatabaseError::MigrationApply { version: 42, .. }
    ));
    assert!(error.to_string().contains("migration v42"));
    let source = std::error::Error::source(&error)
        .expect("MigrationApply must retain the database error source chain");
    assert!(source.to_string().contains("Internal database error"));
    assert!(
        std::error::Error::source(source).is_some(),
        "MigrationApply must retain the underlying sqlx error beneath DatabaseError::Internal"
    );
}

#[tokio::test]
async fn postgres_preflight_restores_timeouts_before_migration_body() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (migration timeout restoration)");
        return;
    };
    let schema = unique_postgres_schema_name("ledger_timeouts");
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("postgres administrator");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let mut timed_url = url::Url::parse(&postgres_url_with_search_path(&database_url, &schema))
        .expect("parse postgres URL");
    timed_url
        .query_pairs_mut()
        .append_pair("options", "-c statement_timeout=20s -c lock_timeout=6s");
    let db = Database::from_config(
        "migration-timeout-restoration",
        &DatabaseConfig::new(DatabaseDriver::Postgres, timed_url.to_string()),
    )
    .await
    .expect("open isolated postgres database");
    let runner = MigrationRunner::new(vec![Migration {
        version: 42,
        description: "capture migration timeouts".to_string(),
        sql_sqlite: "SELECT 1;",
        sql_postgres: "CREATE TABLE timeout_capture (statement_timeout TEXT, lock_timeout TEXT); \
            INSERT INTO timeout_capture VALUES (pg_catalog.current_setting('statement_timeout'), \
            pg_catalog.current_setting('lock_timeout'));",
    }]);
    runner.run(&db).await.expect("run migration");
    let timeouts: (String, String) = sqlx::query_as(&format!(
        "SELECT statement_timeout, lock_timeout FROM {schema}.timeout_capture"
    ))
    .fetch_one(&admin)
    .await
    .expect("read migration-body timeouts");
    assert_eq!(timeouts, ("20s".to_string(), "6s".to_string()));
    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_migration_runner_blocks_until_the_advisory_lock_is_released() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (migration ledger lock)");
        return;
    };
    let schema = unique_postgres_schema_name("ledger_lock");
    let (db_a, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let application = format!("ledger_lock_{}", uuid::Uuid::new_v4().simple());
    let mut scoped_url = url::Url::parse(&postgres_url_with_search_path(&database_url, &schema))
        .expect("parse scoped Postgres URL");
    scoped_url
        .query_pairs_mut()
        .append_pair("application_name", &application);
    let config = DatabaseConfig::new(DatabaseDriver::Postgres, scoped_url.to_string());
    let db_b = Database::from_config("ledger-lock-b", &config)
        .await
        .expect("open scoped migration database");
    let mut lock_tx = db_a.begin().await.expect("start lock transaction");
    lock_tx
        .query(
            "SELECT pg_advisory_xact_lock(?)",
            crate::db_params![super::runner::MIGRATION_LEDGER_ADVISORY_LOCK_KEY],
        )
        .await
        .expect("take migration advisory lock");

    let local = tokio::task::LocalSet::new();
    let mut run = local.spawn_local(async move {
        let runner = MigrationRunner::single();
        runner.run(&db_b).await
    });
    local
        .run_until(async {
            wait_for_postgres_migration_retry(&admin, &application).await;
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut run)
                    .await
                    .is_err(),
                "migration runner must block while another transaction holds its advisory lock"
            );
        })
        .await;
    lock_tx
        .commit()
        .await
        .expect("release migration advisory lock");
    let applied = local
        .run_until(&mut run)
        .await
        .expect("migration runner task")
        .expect("migration runner result");
    assert!(!applied.is_empty());

    drop(db_a);
    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_concurrent_migration_runners_record_one_complete_ledger() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (migration ledger race)");
        return;
    };
    let schema = unique_postgres_schema_name("ledger_race");
    let (db_a, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let db_b = open_postgres_database_in_schema(&database_url, &schema, "ledger-race-b", 10).await;
    let expected: Vec<i64> = MigrationRunner::single()
        .migrations
        .iter()
        .map(|migration| migration.version)
        .collect();
    let first_runner = MigrationRunner::single();
    let second_runner = MigrationRunner::single();
    let (first, second) = tokio::join!(first_runner.run(&db_a), second_runner.run(&db_b));
    let first = first.expect("first migration runner");
    let second = second.expect("second migration runner");
    assert!(
        (first == expected && second.is_empty()) || (second == expected && first.is_empty()),
        "the advisory lock must let exactly one runner apply the full catalog"
    );
    assert_eq!(
        migration_ledger_row_count(&db_a).await,
        expected.len() as i64
    );
    for migration in MigrationRunner::single().migrations {
        let expected_checksum = migration_checksum(&migration, DatabaseDriver::Postgres);
        assert_eq!(
            migration_ledger_checksum(&db_a, migration.version)
                .await
                .as_deref(),
            Some(expected_checksum.as_str()),
            "migration v{} must be recorded once with its active-dialect checksum",
            migration.version
        );
    }

    drop(db_b);
    drop(db_a);
    drop_postgres_schema(&admin, &schema).await;
}

/// Runs without PostgreSQL: the extractor must accept the checked-in
/// ConfigMap exactly as formatted, so a parser/format drift fails here and
/// not only in the Postgres lane.
#[test]
fn ingress_monitoring_configmap_extracts_every_query() {
    let yaml_path = monitoring_configmap_path();
    let yaml = fs::read_to_string(&yaml_path).unwrap_or_else(|error| {
        panic!(
            "read ingress monitoring ConfigMap at {} (is the flake.nix postUnpack copy intact?): {error}",
            yaml_path.display()
        )
    });
    let queries = extract_monitoring_queries(&yaml).expect("extract ingress monitoring queries");
    let names: Vec<&str> = queries.iter().map(|query| query.name.as_str()).collect();
    assert_eq!(names, EXPECTED_MONITORING_QUERIES);
    for query in &queries {
        assert!(
            !query.metrics.is_empty() && query.sql.contains("FROM"),
            "query {} extracted without metrics or SQL",
            query.name
        );
    }
}

#[test]
fn ingress_monitoring_kind_families_match_storage_codes() {
    let yaml = fs::read_to_string(monitoring_configmap_path()).expect("read monitoring ConfigMap");
    let queries = extract_monitoring_queries(&yaml).expect("extract monitoring queries");
    let query = queries
        .iter()
        .find(|query| query.name == "waddle_ingress_nonterminal")
        .expect("non-terminal monitoring query");
    let arms: Vec<(i32, &str)> = query
        .sql
        .lines()
        .filter_map(|line| line.trim().strip_prefix("WHEN "))
        .map(|arm| {
            let (code, name) = arm.split_once(" THEN ").expect("CASE arm shape");
            (
                code.parse().expect("integer storage code"),
                name.strip_prefix('\'')
                    .and_then(|name| name.strip_suffix('\''))
                    .expect("quoted storage family"),
            )
        })
        .collect();
    assert_eq!(
        arms,
        waddle_xmpp::ingress::IngressEffectIntent::storage_kind_names(),
        "monitoring CASE must cover exactly the stable storage-code families"
    );
}

const EXPECTED_MONITORING_QUERIES: [&str; 9] = [
    "waddle_ingress_table",
    "waddle_ingress_gc",
    "waddle_ingress_messages",
    "waddle_ingress_cohort",
    "waddle_ingress_streams",
    "waddle_ingress_nonterminal",
    "waddle_ingress_nonterminal_age",
    "waddle_ingress_room_requests",
    "waddle_ingress_direct_requests",
];

fn monitoring_configmap_path() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("test runner sets CARGO_MANIFEST_DIR")).join(
        "../../../infrastructure/waddle.cloud/gitops/waddle-server/postgresql-monitoring-ingress.yaml",
    )
}

#[derive(Debug)]
struct MonitoringQuery {
    name: String,
    sql: String,
    metrics: Vec<String>,
}

#[tokio::test]
async fn postgres_monitoring_queries_match_migrated_ingress_schema() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (ingress monitoring queries)");
        return;
    };
    let yaml_path = monitoring_configmap_path();
    // Never skip on a missing file: the nix test lanes materialize this
    // path through `testArgs.postUnpack` in flake.nix, and a silent skip
    // would turn this guard green-by-absence (same rule as the Mimir-rules
    // guard in waddle-xmpp).
    let yaml = fs::read_to_string(&yaml_path).unwrap_or_else(|error| {
        panic!(
            "read ingress monitoring ConfigMap at {} (is the flake.nix postUnpack copy intact?): {error}",
            yaml_path.display()
        )
    });
    let queries = extract_monitoring_queries(&yaml).expect("extract ingress monitoring queries");
    let names: Vec<&str> = queries.iter().map(|query| query.name.as_str()).collect();
    assert_eq!(
        names, EXPECTED_MONITORING_QUERIES,
        "the ConfigMap query set changed; update this test's expectations deliberately"
    );

    let schema = unique_postgres_schema_name("monitoring_queries");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("run migrations in isolated postgres schema");
    assert_nonterminal_monitoring_index(&db).await;

    crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
        db.clone(),
        waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
    )
    .await
    .expect("pending delivery schema used by ingress GC monitoring");

    let query_pool = sqlx::PgPool::connect(db.database_url())
        .await
        .expect("connect isolated postgres query pool");
    // CloudNativePG executes custom queries with `SET ROLE pg_monitor`
    // (one transaction per query); mirror that so a missing grant on an
    // ingress table fails here instead of on the production exporter.
    let mut monitor_conn = query_pool
        .acquire()
        .await
        .expect("acquire connection for pg_monitor queries");
    // The fixture lives in an isolated schema; production tables are in
    // `public`, where pg_monitor already has USAGE. Grant the same here so
    // the table-level V1011 grant is what the test exercises.
    sqlx::query(&format!("GRANT USAGE ON SCHEMA {schema} TO pg_monitor"))
        .execute(&admin)
        .await
        .expect("grant fixture schema usage to pg_monitor");
    sqlx::query("SET ROLE pg_monitor")
        .execute(&mut *monitor_conn)
        .await
        .expect("assume pg_monitor like the CNPG exporter");
    for monitoring_query in &queries {
        // The production query pins `schemaname = 'public'`; the fixture
        // migrates into an isolated schema, so point it at that schema to
        // keep the table-name literals under test.
        let sql = if monitoring_query.name == "waddle_ingress_table" {
            assert!(
                monitoring_query.sql.contains("schemaname = 'public'"),
                "waddle_ingress_table must filter on schemaname = 'public'"
            );
            monitoring_query
                .sql
                .replace("schemaname = 'public'", "schemaname = current_schema()")
        } else {
            monitoring_query.sql.clone()
        };
        let rows = sqlx::query(&sql)
            .fetch_all(&mut *monitor_conn)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "execute monitoring query {} against migrated schema: {error}",
                    monitoring_query.name
                )
            });
        assert_declared_metric_columns(monitoring_query, &rows);

        match monitoring_query.name.as_str() {
            "waddle_ingress_gc"
            | "waddle_ingress_streams"
            | "waddle_ingress_room_requests"
            | "waddle_ingress_direct_requests" => assert_eq!(
                rows.len(),
                1,
                "{} must return one aggregate row for an empty ingress schema",
                monitoring_query.name
            ),
            "waddle_ingress_cohort" => {
                let states: Vec<(String, i64)> = rows
                    .iter()
                    .map(|row| {
                        (
                            row.try_get("state").expect("decode cohort state"),
                            row.try_get("count").expect("decode cohort count"),
                        )
                    })
                    .collect();
                assert_eq!(
                    states,
                    vec![
                        ("live".to_string(), 0),
                        ("terminal_referenced".to_string(), 0),
                        ("terminal_unreferenced".to_string(), 0),
                    ],
                    "waddle_ingress_cohort must report each empty lifecycle state"
                );
            }
            "waddle_ingress_table" => {
                let tables: Vec<String> = rows
                    .iter()
                    .map(|row| row.try_get("table").expect("decode table name"))
                    .collect();
                assert_eq!(
                    tables,
                    [
                        "ingress_archive_dispatch",
                        "ingress_deliveries",
                        "ingress_effect_descendants",
                        "ingress_effect_intents",
                        "ingress_effect_receipts",
                        "ingress_messages",
                        "ingress_origin_aliases",
                        "ingress_owner_rejections",
                        "ingress_sm_refs",
                        "ingress_sm_streams",
                    ],
                    "every monitored ingress table must exist in the migrated schema"
                );
            }
            "waddle_ingress_nonterminal" => {
                assert_eq!(rows.len(), 1, "empty backlog must still emit the sentinel");
                let kind: String = rows[0].try_get("kind").expect("sentinel kind");
                let messages: i64 = rows[0].try_get("messages").expect("sentinel count");
                assert_eq!((kind.as_str(), messages), ("none", 0));
            }
            "waddle_ingress_nonterminal_age" => {
                assert_eq!(rows.len(), 1, "age query must always emit one row");
                let age: f64 = sqlx::query_scalar(&format!(
                    "SELECT oldest_seconds::double precision FROM ({}) AS age",
                    sql.trim().trim_end_matches(';')
                ))
                .fetch_one(&mut *monitor_conn)
                .await
                .expect("decode empty non-terminal age");
                assert_eq!(age, 0.0);
            }
            "waddle_ingress_messages" => {}
            other => panic!("unexpected ingress monitoring query {other}"),
        }
    }

    assert_pending_archive_reference_monitoring(&query_pool, &mut monitor_conn, &queries).await;
    assert_descendant_reference_monitoring(&query_pool, &mut monitor_conn, &queries).await;
    assert_populated_nonterminal_monitoring(&query_pool, &mut monitor_conn, &queries).await;

    drop(monitor_conn);
    query_pool.close().await;
    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

async fn assert_pending_archive_reference_monitoring(
    pool: &sqlx::PgPool,
    monitor: &mut sqlx::PgConnection,
    queries: &[MonitoringQuery],
) {
    // Both rows are expired, fully receipted, and inside the configured cohort.
    // Only the first dispatch still points to an extant pending delivery.
    sqlx::raw_sql(
        "INSERT INTO ingress_messages (message_key, digest_version, digest, created_at, terminal_at)
         VALUES
           ('00000000-0000-0000-0000-000000000011', 1, decode(repeat('11', 32), 'hex'), '1970-01-01T01:00:00Z', '1970-01-01T02:00:00Z'),
           ('00000000-0000-0000-0000-000000000012', 1, decode(repeat('12', 32), 'hex'), '1970-01-01T01:00:00Z', '1970-01-01T02:00:00Z');
         INSERT INTO ingress_effect_intents
           (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload)
         SELECT message_key, 0, 22, digest, 1, '{}'::bytea FROM ingress_messages;
         INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash)
         SELECT message_key, kind, semantic_identity_hash FROM ingress_effect_intents;
         INSERT INTO ingress_archive_dispatch
           (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id)
         SELECT 'juliet@example.com', 1, message_key, 22, digest, '', message_key::text
         FROM ingress_messages;
         INSERT INTO pending_delivery
           (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id)
         VALUES ('00000000-0000-0000-0000-000000000011', 'juliet@example.com', 3600000, 'archived', 'juliet@example.com', 'monitoring-archive');",
    )
    .execute(pool)
    .await
    .expect("insert pending archive monitoring fixture");

    for retained in [true, false] {
        let referenced = i64::from(retained);
        let unreferenced = 2 - referenced;
        for name in ["waddle_ingress_messages", "waddle_ingress_cohort"] {
            let query = queries
                .iter()
                .find(|query| query.name == name)
                .expect("lifecycle monitoring query");
            let rows = sqlx::query(&query.sql)
                .fetch_all(&mut *monitor)
                .await
                .expect("query pending archive lifecycle as pg_monitor");
            let counts: std::collections::BTreeMap<String, i64> = rows
                .iter()
                .map(|row| {
                    (
                        row.try_get("state").expect("lifecycle state"),
                        row.try_get("count").expect("lifecycle count"),
                    )
                })
                .collect();
            assert_eq!(
                counts.get("terminal_referenced").copied().unwrap_or(0),
                referenced,
                "{name}: only extant pending rows retain a terminal reference"
            );
            assert_eq!(
                counts.get("terminal_unreferenced"),
                Some(&unreferenced),
                "{name}: dangling dispatch references must not retain messages"
            );
        }
        let gc = queries
            .iter()
            .find(|query| query.name == "waddle_ingress_gc")
            .expect("GC monitoring query");
        let row = sqlx::query(&gc.sql)
            .fetch_one(&mut *monitor)
            .await
            .expect("query pending archive GC as pg_monitor");
        assert_eq!(
            row.try_get::<i64, _>("eligible_messages")
                .expect("eligible count"),
            unreferenced
        );
        assert_eq!(
            row.try_get::<i64, _>("retained_referenced_messages")
                .expect("retained count"),
            referenced
        );
        if retained {
            sqlx::query("DELETE FROM pending_delivery")
                .execute(pool)
                .await
                .expect("release pending archive reference");
        }
    }
}

async fn assert_descendant_reference_monitoring(
    pool: &sqlx::PgPool,
    monitor: &mut sqlx::PgConnection,
    queries: &[MonitoringQuery],
) {
    // V1025 can retain observer publications with descendants alone: no SM,
    // delivery binding or archive dispatch exists for this terminal parent.
    sqlx::raw_sql(
        "INSERT INTO ingress_messages (message_key, digest_version, digest, created_at, terminal_at)
         VALUES ('00000000-0000-0000-0000-000000000013', 1, decode(repeat('13', 32), 'hex'), '1970-01-01T01:00:00Z', '1970-01-01T02:00:00Z');
         INSERT INTO ingress_effect_intents
           (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload)
         SELECT message_key, 0, 28, digest, 1, '{}'::bytea FROM ingress_messages
         WHERE message_key = '00000000-0000-0000-0000-000000000013';
         INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash)
         SELECT message_key, kind, semantic_identity_hash FROM ingress_effect_intents
         WHERE message_key = '00000000-0000-0000-0000-000000000013';
         INSERT INTO ingress_effect_descendants
           (message_key, kind, semantic_identity_hash, descendant_key)
         SELECT message_key, kind, semantic_identity_hash, 'monitoring-observer-publication'
         FROM ingress_effect_intents
         WHERE message_key = '00000000-0000-0000-0000-000000000013';",
    )
    .execute(pool)
    .await
    .expect("insert descendant-only observer monitoring fixture");

    for unsettled in [true, false] {
        let referenced = i64::from(unsettled);
        // The two prior pending archive fixtures have already been released.
        let unreferenced = 3 - referenced;
        for name in ["waddle_ingress_messages", "waddle_ingress_cohort"] {
            let query = queries
                .iter()
                .find(|query| query.name == name)
                .expect("lifecycle query");
            let rows = sqlx::query(&query.sql)
                .fetch_all(&mut *monitor)
                .await
                .expect("query descendant lifecycle as pg_monitor");
            let counts: std::collections::BTreeMap<String, i64> = rows
                .iter()
                .map(|row| {
                    (
                        row.try_get("state").expect("lifecycle state"),
                        row.try_get("count").expect("lifecycle count"),
                    )
                })
                .collect();
            assert_eq!(
                counts.get("terminal_referenced").copied().unwrap_or(0),
                referenced,
                "{name}: unsettled observer descendants retain their terminal parent"
            );
            assert_eq!(
                counts.get("terminal_unreferenced"),
                Some(&unreferenced),
                "{name}: settled descendants stop retaining their terminal parent"
            );
        }
        let gc = queries
            .iter()
            .find(|query| query.name == "waddle_ingress_gc")
            .expect("GC query");
        let row = sqlx::query(&gc.sql)
            .fetch_one(&mut *monitor)
            .await
            .expect("query descendant GC as pg_monitor");
        assert_eq!(
            row.try_get::<i64, _>("eligible_messages")
                .expect("eligible count"),
            unreferenced
        );
        assert_eq!(
            row.try_get::<i64, _>("retained_referenced_messages")
                .expect("retained count"),
            referenced
        );
        if unsettled {
            sqlx::query("UPDATE ingress_effect_descendants SET settled_at = now() WHERE descendant_key = 'monitoring-observer-publication'")
                .execute(pool).await.expect("settle observer descendant");
        }
    }
}

async fn assert_populated_nonterminal_monitoring(
    pool: &sqlx::PgPool,
    monitor: &mut sqlx::PgConnection,
    queries: &[MonitoringQuery],
) {
    // Write as the application fixture owner, then read only as pg_monitor.
    // Epoch zero permits these writes without a protocol-epoch guard token.
    sqlx::raw_sql(
        "INSERT INTO ingress_messages (message_key, digest_version, digest, created_at, terminal_at)
         VALUES
           ('00000000-0000-0000-0000-000000000001', 1, decode(repeat('01', 32), 'hex'), now() - interval '1 minute', NULL),
           ('00000000-0000-0000-0000-000000000002', 1, decode(repeat('02', 32), 'hex'), now() - interval '2 hours', now()),
           ('00000000-0000-0000-0000-000000000003', 1, decode(repeat('03', 32), 'hex'), now() - interval '1 hour', NULL),
           ('00000000-0000-0000-0000-000000000004', 1, decode(repeat('04', 32), 'hex'), now() - interval '30 minutes', NULL);
         INSERT INTO ingress_effect_intents
           (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload)
         SELECT message_key::uuid, ordinal, kind, decode(repeat(identity, 32), 'hex'), 1, '{}'::bytea
         FROM (VALUES
           ('00000000-0000-0000-0000-000000000001', 0, 2, '01'),
           ('00000000-0000-0000-0000-000000000002', 0, 2, '01'),
           ('00000000-0000-0000-0000-000000000003', 0, 2, '01'),
           ('00000000-0000-0000-0000-000000000004', 0, 2, '01'),
           ('00000000-0000-0000-0000-000000000004', 1, 2, '02'),
           ('00000000-0000-0000-0000-000000000004', 2, 7, '03')
         ) AS fixture(message_key, ordinal, kind, identity);
         INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash)
         SELECT message_key, kind, semantic_identity_hash FROM ingress_effect_intents
         WHERE message_key = '00000000-0000-0000-0000-000000000003';",
    )
    .execute(pool)
    .await
    .expect("insert non-terminal monitoring fixture");
    let backlog = queries
        .iter()
        .find(|query| query.name == "waddle_ingress_nonterminal")
        .expect("non-terminal query");
    let rows = sqlx::query(&backlog.sql)
        .fetch_all(&mut *monitor)
        .await
        .expect("query populated backlog as pg_monitor");
    assert_declared_metric_columns(backlog, &rows);
    let mut counts: Vec<(String, i64)> = rows
        .iter()
        .map(|row| {
            (
                row.try_get("kind").expect("pending kind family"),
                row.try_get("messages")
                    .expect("distinct canonical messages"),
            )
        })
        .collect();
    counts.sort();
    assert_eq!(
        counts,
        vec![
            ("none".to_string(), 0),
            ("notification_activity_preview".to_string(), 1),
            ("route_muc".to_string(), 1),
            ("terminalization".to_string(), 1),
        ]
    );
    let age = queries
        .iter()
        .find(|query| query.name == "waddle_ingress_nonterminal_age")
        .expect("age query");
    let ages: Vec<f64> = sqlx::query_scalar(&format!(
        "SELECT oldest_seconds::double precision FROM ({}) AS age",
        age.sql.trim().trim_end_matches(';')
    ))
    .fetch_all(&mut *monitor)
    .await
    .expect("query populated age as pg_monitor");
    assert_eq!(ages.len(), 1);
    assert!(
        ages[0] >= 3600.0 && ages[0] < 3900.0,
        "oldest age: {ages:?}"
    );
    let plan: Vec<String> = sqlx::query_scalar(&format!("EXPLAIN {}", backlog.sql))
        .fetch_all(&mut *monitor)
        .await
        .expect("explain backlog as pg_monitor");
    eprintln!(
        "non-terminal monitoring fixture EXPLAIN:\n{}",
        plan.join("\n")
    );
}

fn assert_declared_metric_columns(query: &MonitoringQuery, rows: &[sqlx::postgres::PgRow]) {
    let Some(row) = rows.first() else {
        return;
    };
    let columns: HashSet<&str> = row.columns().iter().map(|column| column.name()).collect();
    for metric in &query.metrics {
        assert!(
            columns.contains(metric.as_str()),
            "{} result is missing declared metric column {metric}; got {columns:?}",
            query.name
        );
    }
}

/// Extract the fixed CNPG ConfigMap shape without a YAML dependency.
///
/// The `queries` literal must use four-space query names, an exact six-space
/// `query: |` field whose SQL is indented eight spaces, and an exact
/// six-space `metrics:` field whose metric maps begin with eight-space
/// `- <column>:` entries. Formatting that changes those anchors is rejected
/// so no query can be silently omitted from schema-drift coverage.
fn extract_monitoring_queries(yaml: &str) -> Result<Vec<MonitoringQuery>, String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let queries_start = lines
        .iter()
        .position(|line| *line == "  queries: |")
        .ok_or("missing exact ConfigMap `  queries: |` block")?;
    let query_end = lines[queries_start + 1..]
        .iter()
        .position(|line| !line.is_empty() && !line.starts_with(' '))
        .map_or(lines.len(), |offset| queries_start + 1 + offset);

    let mut headers = Vec::new();
    for (index, &line) in lines
        .iter()
        .enumerate()
        .take(query_end)
        .skip(queries_start + 1)
    {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with('\t') {
            return Err(format!("line {} uses tab indentation", index + 1));
        }
        if indentation(line) == 4 {
            let name = line
                .strip_prefix("    ")
                .and_then(|value| value.strip_suffix(':'))
                .filter(|value| !value.is_empty() && !value.contains(char::is_whitespace))
                .ok_or_else(|| format!("line {} is not a four-space query name", index + 1))?;
            headers.push((index, name));
        }
    }
    if headers.is_empty() {
        return Err("ConfigMap queries block contains no four-space query names".to_string());
    }
    if let Some((first_header, _)) = headers.first() {
        for (offset, line) in lines[queries_start + 1..*first_header].iter().enumerate() {
            if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
                return Err(format!(
                    "queries block has content before its first query name at line {}",
                    queries_start + 2 + offset
                ));
            }
        }
    }

    headers
        .iter()
        .enumerate()
        .map(|(header_index, &(start, name))| {
            let end = headers
                .get(header_index + 1)
                .map_or(query_end, |(next, _)| *next);
            extract_query_block(&lines, start, end, name)
        })
        .collect()
}

fn extract_query_block(
    lines: &[&str],
    start: usize,
    end: usize,
    name: &str,
) -> Result<MonitoringQuery, String> {
    let query_fields: Vec<usize> = (start + 1..end)
        .filter(|&index| lines[index] == "      query: |")
        .collect();
    let metrics_fields: Vec<usize> = (start + 1..end)
        .filter(|&index| lines[index] == "      metrics:")
        .collect();
    if query_fields.len() != 1 || metrics_fields.len() != 1 {
        return Err(format!(
            "query {name} must contain exactly one `      query: |` and one `      metrics:` field"
        ));
    }

    let query_start = query_fields[0];
    let metrics_start = metrics_fields[0];
    if metrics_start <= query_start {
        return Err(format!("query {name} places metrics before its SQL"));
    }
    for (offset, line) in lines[start + 1..query_start].iter().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with("      ") || line.starts_with("        ") {
            return Err(format!(
                "query {name} has malformed field indentation before SQL at line {}",
                start + 2 + offset
            ));
        }
    }
    let sql_end = (query_start + 1..end)
        .find(|&index| lines[index].starts_with("      ") && !lines[index].starts_with("        "))
        .unwrap_or(end);
    if sql_end > metrics_start {
        return Err(format!("query {name} has no field after its SQL literal"));
    }

    let mut sql = Vec::new();
    for (offset, line) in lines[query_start + 1..sql_end].iter().enumerate() {
        if line.is_empty() {
            sql.push(String::new());
        } else if let Some(sql_line) = line.strip_prefix("        ") {
            sql.push(sql_line.to_string());
        } else {
            return Err(format!(
                "query {name} has non-eight-space SQL indentation at line {}",
                query_start + 2 + offset
            ));
        }
    }
    if sql.iter().all(|line| line.trim().is_empty()) {
        return Err(format!("query {name} has an empty SQL literal"));
    }
    for (offset, line) in lines[sql_end..metrics_start].iter().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with("      ") || line.starts_with("        ") {
            return Err(format!(
                "query {name} has malformed field indentation before metrics at line {}",
                sql_end + 1 + offset
            ));
        }
    }

    let mut metrics = Vec::new();
    for (offset, line) in lines[metrics_start + 1..end].iter().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(metric) = line.strip_prefix("        - ") {
            let metric = metric
                .strip_suffix(':')
                .filter(|metric| !metric.is_empty() && !metric.contains(char::is_whitespace))
                .ok_or_else(|| {
                    format!(
                        "query {name} has malformed metric entry at line {}",
                        metrics_start + 2 + offset
                    )
                })?;
            if !metrics.iter().all(|existing| existing != metric) {
                return Err(format!(
                    "query {name} declares metric {metric} more than once"
                ));
            }
            metrics.push(metric.to_string());
        } else if line.starts_with("            ") && !metrics.is_empty() {
            continue;
        } else {
            return Err(format!(
                "query {name} has malformed metrics indentation at line {}",
                metrics_start + 2 + offset
            ));
        }
    }
    if metrics.is_empty() {
        return Err(format!("query {name} declares no metric columns"));
    }

    Ok(MonitoringQuery {
        name: name.to_string(),
        sql: sql.join("\n"),
        metrics,
    })
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

#[tokio::test]
async fn postgres_migration_runner_succeeds_with_a_single_connection_pool() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (single connection migration pool)");
        return;
    };
    let schema = unique_postgres_schema_name("ledger_pool_one");
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect postgres admin pool");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create isolated postgres schema");
    let db = open_postgres_database_in_schema(&database_url, &schema, "ledger-pool-one", 1).await;

    assert!(!MigrationRunner::single()
        .run(&db)
        .await
        .expect("run migrations with one pooled connection")
        .is_empty());

    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

async fn migration_ledger_row_count(db: &Database) -> i64 {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query("SELECT COUNT(*) FROM _migrations", ())
        .await
        .expect("query migration ledger count");
    let row = rows
        .next()
        .await
        .expect("read migration ledger count")
        .expect("migration ledger count row");
    row.get(0).expect("decode migration ledger count")
}

async fn migration_ledger_checksum(db: &Database, version: i64) -> Option<String> {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT checksum FROM _migrations WHERE version = ?",
            crate::db_params![version],
        )
        .await
        .expect("query migration ledger checksum");
    let row = rows
        .next()
        .await
        .expect("read migration ledger checksum")
        .expect("migration ledger checksum row");
    row.get(0).expect("decode migration ledger checksum")
}

async fn open_isolated_postgres_database(
    database_url: &str,
    schema: &str,
) -> (Database, sqlx::PgPool) {
    let admin = sqlx::PgPool::connect(database_url)
        .await
        .expect("connect postgres admin pool");
    let create_schema = format!("CREATE SCHEMA {schema}");
    sqlx::query(&create_schema)
        .execute(&admin)
        .await
        .expect("create isolated postgres schema");

    let db = open_postgres_database_in_schema(
        database_url,
        schema,
        "isolated-postgres-migration-test",
        10,
    )
    .await;
    (db, admin)
}

async fn open_postgres_database_in_schema(
    database_url: &str,
    schema: &str,
    name: &str,
    pool_size: u32,
) -> Database {
    let scoped_url = postgres_url_with_search_path(database_url, schema);
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, scoped_url);
    config.pool_size = pool_size;
    Database::from_config(name, &config)
        .await
        .expect("open isolated postgres database")
}

async fn drop_postgres_schema(admin: &sqlx::PgPool, schema: &str) {
    let drop_schema = format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
    sqlx::query(&drop_schema)
        .execute(admin)
        .await
        .expect("drop isolated postgres schema");
}

async fn wait_for_postgres_migration_retry(admin: &sqlx::PgPool, application: &str) {
    let retry = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let idle: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.state = 'idle' \
                   AND a.query = 'ROLLBACK' AND a.xact_start IS NULL \
                   AND a.backend_xmin IS NULL AND NOT EXISTS (SELECT 1 \
                     FROM pg_catalog.pg_locks l WHERE l.pid = a.pid \
                       AND l.locktype = 'advisory'))",
            )
            .bind(application)
            .fetch_one(admin)
            .await
            .expect("inspect retrying migration session");
            if idle {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    })
    .await;
    assert!(
        retry.is_ok(),
        "migration runner must retry without a transaction or advisory lock"
    );
}

fn unique_postgres_schema_name(prefix: &str) -> String {
    format!("waddle_test_{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres url");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

async fn assert_nonterminal_monitoring_index(db: &Database) {
    let (query, expected_suffix) = match db.driver() {
        DatabaseDriver::Sqlite => (
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'ingress_messages_nonterminal_created_at_idx'",
            "(created_at) WHERE terminal_at IS NULL",
        ),
        DatabaseDriver::Postgres => (
            "SELECT indexdef FROM pg_indexes WHERE schemaname = current_schema() AND indexname = 'ingress_messages_nonterminal_created_at_idx'",
            "(created_at) WHERE (terminal_at IS NULL)",
        ),
    };
    let conn = db.guard().await.expect("index catalog connection");
    let mut rows = conn
        .query(query, ())
        .await
        .expect("query non-terminal index");
    let definition: String = rows
        .next()
        .await
        .expect("index catalog result")
        .expect("non-terminal index exists")
        .get(0)
        .expect("index definition");
    assert!(
        definition.ends_with(expected_suffix),
        "index definition: {definition}"
    );
}

#[tokio::test]
async fn postgres_observer_inspection_queries_run_as_pg_monitor() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (observer inspection queries)");
        return;
    };
    let statements = runbook_statements(&runbook_sql_block("observer-retention-inspection"));
    for table in OBSERVER_TABLES {
        assert!(
            statements.iter().any(|statement| statement.contains(table)),
            "the inspection block reads {table}"
        );
    }
    let schema = unique_postgres_schema_name("observer_inspection");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("run migrations in isolated postgres schema");
    crate::ingress_uow::initialize_room_observations(&db)
        .await
        .expect("startup DDL after V1023");
    // Production tables live in `public`, where pg_monitor already has USAGE.
    sqlx::query(&format!("GRANT USAGE ON SCHEMA {schema} TO pg_monitor"))
        .execute(&admin)
        .await
        .expect("grant fixture schema usage to pg_monitor");
    let pool = sqlx::PgPool::connect(db.database_url())
        .await
        .expect("connect monitor pool");
    let mut monitor = pool.acquire().await.expect("monitor connection");
    sqlx::query("SET ROLE pg_monitor")
        .execute(&mut *monitor)
        .await
        .expect("assume pg_monitor");
    for statement in &statements {
        sqlx::query(statement)
            .fetch_all(&mut *monitor)
            .await
            .unwrap_or_else(|error| panic!("pg_monitor cannot run {statement}: {error}"));
    }
    drop(monitor);
    pool.close().await;
    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

const OBSERVER_TABLES: [&str; 6] = [
    "extension_room_observers",
    "extension_room_sources",
    "extension_room_source_revisions",
    "extension_room_observation_work",
    "extension_room_observation_receipts",
    "extension_room_publications",
];
