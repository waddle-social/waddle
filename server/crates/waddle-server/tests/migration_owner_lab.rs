//! Real PostgreSQL characterization for the unwired migration-owner lab.

#[path = "../src/test_support/postgres_proxy.rs"]
pub mod postgres_proxy;

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{self, AttestedLineage, DeploymentUuid};
use waddle_server::db::physical_attestation::PgRoleName;
use waddle_server::db::{
    run_dark_postgres_owner, DarkMigrationOwnerConfig, DarkMigrationOwnerError, Database,
    DatabaseConfig, DatabaseDriver, MigrationRunner,
};

#[path = "migration_owner_lab/catalog_fence.rs"]
mod catalog_fence;
#[path = "migration_owner_lab/epoch_acl.rs"]
mod epoch_acl;
#[cfg(feature = "test-support")]
#[path = "migration_owner_lab/mam.rs"]
mod mam;
#[path = "migration_owner_lab/serving_role_graph.rs"]
mod serving_role_graph;
#[cfg(feature = "test-support")]
#[path = "migration_owner_lab/transition.rs"]
mod transition;

struct Fixture {
    cluster: PgPool,
    admin: PgPool,
    admin_options: PgConnectOptions,
    migrator_options: PgConnectOptions,
    runtime_options: PgConnectOptions,
    database: String,
    schema: String,
    owner: String,
    migrator: String,
    runtime: String,
    lineage: AttestedLineage,
}

impl Fixture {
    async fn new(url: &str, migrate: bool) -> Self {
        let cluster = PgPool::connect(url).await.expect("cluster administrator");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("migration_lab_db_{suffix}");
        let schema = format!("migration_lab_s_{suffix}");
        let owner = format!("migration_lab_o_{suffix}");
        let migrator = format!("migration_lab_m_{suffix}");
        let runtime = format!("migration_lab_r_{suffix}");
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&cluster)
            .await
            .expect("create isolated database");
        for role in [&owner, &migrator, &runtime] {
            sqlx::query(&format!("CREATE ROLE {role} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"))
                .execute(&cluster).await.expect("create separated login");
        }
        let base = url_with_database(url, &database);
        let admin = PgPool::connect(&base)
            .await
            .expect("database administrator");
        for statement in [
            format!("CREATE SCHEMA {schema} AUTHORIZATION {owner}"),
            format!("GRANT USAGE ON SCHEMA {schema} TO {runtime}"),
            format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("provision lab roles");
        }
        let scoped = url_with_option(&base, "options", &format!("-c search_path={schema}"));
        let owner_url = url_with_option(&scoped, "user", &owner);
        let admin_options = PgConnectOptions::from_str(&scoped).expect("admin options");
        let owner_options = PgConnectOptions::from_str(&owner_url).expect("owner options");
        let migrator_options = owner_options.clone().username(&migrator);
        let runtime_options = owner_options.clone().username(&runtime);
        let db = Database::from_config(
            "migration owner fixture",
            &DatabaseConfig::new(DatabaseDriver::Postgres, owner_url),
        )
        .await
        .expect("owner database");
        let lineage = lineage::enroll(
            &db,
            &LineageConfig {
                deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
                action: None,
            },
        )
        .await
        .expect("enroll owner lineage");
        if migrate {
            MigrationRunner::single()
                .run(&db)
                .await
                .expect("baseline actual migrations");
        }
        drop(db);
        Self {
            cluster,
            admin,
            admin_options,
            migrator_options,
            runtime_options,
            database,
            schema,
            owner,
            migrator,
            runtime,
            lineage,
        }
    }

    fn config(&self) -> DarkMigrationOwnerConfig {
        DarkMigrationOwnerConfig::new(
            self.lineage.clone(),
            PgRoleName::new(&self.owner).expect("owner name"),
            PgRoleName::new(&self.runtime).expect("runtime name"),
        )
        .expect("owner configuration")
    }

    fn migrator_role(&self) -> PgRoleName {
        PgRoleName::new(&self.migrator).expect("migrator name")
    }

    async fn enable_owner_handoff(&self) {
        for statement in [
            format!("REVOKE TEMP ON DATABASE {} FROM PUBLIC", self.database),
            format!("ALTER ROLE {} NOLOGIN", self.owner),
            format!(
                "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
                self.owner, self.migrator
            ),
        ] {
            sqlx::query(&statement)
                .execute(&self.admin)
                .await
                .expect("provision three-role handoff");
        }
    }

    async fn cleanup(self) {
        self.admin.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.cluster)
            .await
            .expect("drop test database");
        for role in [&self.runtime, &self.migrator, &self.owner] {
            sqlx::query(&format!("DROP ROLE {role}"))
                .execute(&self.cluster)
                .await
                .expect("drop test role");
        }
        self.cluster.close().await;
    }
}

fn url_with_database(base: &str, database: &str) -> String {
    url_with_option(base, "dbname", database)
}

fn url_with_option(base: &str, key: &str, value: &str) -> String {
    let mut url = url::Url::parse(base).expect("test PostgreSQL URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(existing, _)| existing != key)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(k, v)| (k, v)))
        .append_pair(key, value);
    url.to_string()
}

async fn blocked_by(pool: &PgPool, waiting_pid: i32, blocker_pid: i32) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let blocked: bool =
                sqlx::query_scalar("SELECT $1 = ANY(pg_catalog.pg_blocking_pids($2))")
                    .bind(blocker_pid)
                    .bind(waiting_pid)
                    .fetch_one(pool)
                    .await
                    .expect("inspect wait graph");
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("expected PostgreSQL wait edge");
}

async fn wait_for_application(pool: &PgPool, application: &str, blocker_pid: i32) -> i32 {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_stat_activity \
                 WHERE application_name = $1 AND $2 = ANY(pg_catalog.pg_blocking_pids(pid))",
            )
            .bind(application)
            .bind(blocker_pid)
            .fetch_optional(pool)
            .await
            .expect("find owner wait");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owner blocked on ledger")
}

#[tokio::test]
async fn owner_refuses_missing_epoch_wrong_identity_and_runtime_role_escalation() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, false).await;
    fixture.enable_owner_handoff().await;
    assert!(matches!(
        run_dark_postgres_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role()
        )
        .await,
        Err(DarkMigrationOwnerError::MissingEpochBaseline)
    ));
    let mut wrong = fixture.lineage.clone();
    wrong
        .postgres_identity
        .as_mut()
        .expect("postgres identity")
        .schema
        .oid += 1;
    let wrong = DarkMigrationOwnerConfig::new(
        wrong,
        PgRoleName::new(&fixture.owner).expect("owner"),
        PgRoleName::new(&fixture.runtime).expect("runtime"),
    )
    .expect("config");
    assert!(matches!(
        run_dark_postgres_owner(&fixture.migrator_options, &wrong, &fixture.migrator_role()).await,
        Err(DarkMigrationOwnerError::Identity)
    ));
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    assert!(sqlx::query(&format!(
        "CREATE TABLE {}.forbidden (id int)",
        fixture.schema
    ))
    .execute(&mut runtime)
    .await
    .is_err());
    assert!(sqlx::query(&format!(
        "ALTER TABLE {}._lineage ADD COLUMN forbidden int",
        fixture.schema
    ))
    .execute(&mut runtime)
    .await
    .is_err());
    for role in [&fixture.owner, &fixture.migrator] {
        assert!(
            sqlx::query(&format!("SET ROLE {role}"))
                .execute(&mut runtime)
                .await
                .is_err(),
            "runtime must not assume {role}"
        );
    }
    drop(runtime);
    sqlx::query(&format!(
        "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
        fixture.owner, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("model runtime SET ROLE escalation");
    assert!(matches!(
        run_dark_postgres_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role()
        )
        .await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    sqlx::query(&format!(
        "REVOKE {} FROM {}",
        fixture.owner, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove test escalation");
    fixture.cleanup().await;
}

#[tokio::test]
async fn old_trigger_can_invert_lock_order_against_an_epoch_first_migrator() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    let mut writer = fixture.admin.begin().await.expect("old writer");
    sqlx::query("SET LOCAL deadlock_timeout = '5s'")
        .execute(&mut *writer)
        .await
        .expect("observation window");
    sqlx::query(&format!(
        "LOCK TABLE {}.ingress_messages IN ROW EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *writer)
    .await
    .expect("old target lock");
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await
        .expect("writer PID");
    let mut owner = fixture.admin.begin().await.expect("migrator");
    sqlx::query("SET LOCAL deadlock_timeout = '5s'")
        .execute(&mut *owner)
        .await
        .expect("observation window");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *owner)
    .await
    .expect("epoch-first owner");
    let owner_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *owner)
        .await
        .expect("owner PID");
    let writer_sql = format!(
        "DELETE FROM {}.ingress_messages WHERE false",
        fixture.schema
    );
    let writer_task = tokio::spawn(async move {
        let result = sqlx::query(&writer_sql).execute(&mut *writer).await;
        writer.rollback().await.expect("rollback writer");
        result
    });
    blocked_by(&fixture.admin, writer_pid, owner_pid).await;
    let owner_sql = format!(
        "ALTER TABLE {}.ingress_messages ADD COLUMN lab_marker int",
        fixture.schema
    );
    let owner_task = tokio::spawn(async move {
        let result = sqlx::query(&owner_sql).execute(&mut *owner).await;
        owner.rollback().await.expect("rollback owner");
        result
    });
    blocked_by(&fixture.admin, owner_pid, writer_pid).await;
    let cancelled: bool = sqlx::query_scalar("SELECT pg_catalog.pg_cancel_backend($1)")
        .bind(owner_pid)
        .fetch_one(&fixture.admin)
        .await
        .expect("cancel test owner");
    assert!(cancelled);
    assert!(owner_task.await.expect("owner task").is_err());
    writer_task
        .await
        .expect("writer task")
        .expect("writer unblocked");
    fixture.cleanup().await;
}

#[tokio::test]
async fn experimental_guard_requires_recorded_access_share_even_with_access_exclusive() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    let mut admin = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("admin session");
    sqlx::raw_sql(include_str!(
        "../src/db/migrations/owner/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&mut admin)
    .await
    .expect("install experimental guard");
    let mut only_exclusive = admin.begin().await.expect("first owner");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *only_exclusive)
    .await
    .expect("exclusive lock");
    sqlx::query("SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)")
        .execute(&mut *only_exclusive).await.expect("epoch proof");
    let error = sqlx::query(&format!(
        "DELETE FROM {}.ingress_messages WHERE false",
        fixture.schema
    ))
    .execute(&mut *only_exclusive)
    .await
    .expect_err("exclusive lock alone must fail the candidate guard");
    assert!(error
        .to_string()
        .contains("guarded writes require an epoch ACCESS SHARE prelock"));
    only_exclusive
        .rollback()
        .await
        .expect("rollback rejected write");
    let mut both = admin.begin().await.expect("second owner");
    for mode in ["ACCESS EXCLUSIVE", "ACCESS SHARE"] {
        sqlx::query(&format!(
            "LOCK TABLE ONLY {}.ingress_protocol_epoch IN {mode} MODE",
            fixture.schema
        ))
        .execute(&mut *both)
        .await
        .expect("record lock mode");
    }
    sqlx::query("SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)")
        .execute(&mut *both).await.expect("epoch proof");
    sqlx::query(&format!(
        "DELETE FROM {}.ingress_messages WHERE false",
        fixture.schema
    ))
    .execute(&mut *both)
    .await
    .expect("both locks satisfy guard");
    both.rollback().await.expect("rollback lab write");
    drop(admin);
    fixture.cleanup().await;
}

#[tokio::test]
async fn legacy_guard_requires_epoch_update_privilege_that_can_advance_epoch() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    for statement in [
        format!(
            "GRANT SELECT ON {}.ingress_protocol_epoch TO {}",
            fixture.schema, fixture.runtime
        ),
        format!(
            "GRANT INSERT ON {}.ingress_messages TO {}",
            fixture.schema, fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("runtime write grants");
    }
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    let insert = format!(
        "INSERT INTO {}.ingress_messages (message_key, digest_version, digest) VALUES ($1, 1, $2)",
        fixture.schema,
    );
    let error = sqlx::query(&insert)
        .bind(uuid::Uuid::new_v4())
        .bind([0_u8; 32].as_slice())
        .execute(&mut runtime)
        .await
        .expect_err("SELECT alone cannot satisfy legacy FOR SHARE");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42501")
    );
    sqlx::query(&format!(
        "GRANT UPDATE ON {}.ingress_protocol_epoch TO {}",
        fixture.schema, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant legacy row-lock privilege");
    sqlx::query(&insert)
        .bind(uuid::Uuid::new_v4())
        .bind([0_u8; 32].as_slice())
        .execute(&mut runtime)
        .await
        .expect("legacy guarded write now succeeds");
    let update = format!("UPDATE {}.ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = $1 WHERE id = 1", fixture.schema);
    assert_eq!(
        sqlx::query(&update)
            .bind(fixture.lineage.lineage_uuid.0)
            .execute(&mut runtime)
            .await
            .expect("runtime can also advance epoch")
            .rows_affected(),
        1
    );
    drop(runtime);
    fixture.cleanup().await;
}

#[tokio::test]
async fn sqlite_runner_still_applies_and_rechecks_the_catalog() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
    config.pool_size = 1;
    let db = Database::from_config("sqlite migration regression", &config)
        .await
        .expect("open SQLite");
    let runner = MigrationRunner::single();
    let applied = runner.run(&db).await.expect("apply SQLite catalog");
    assert_eq!(applied, vec![waddle_server::db::BASELINE_VERSION]);
    assert!(runner.run(&db).await.expect("recheck catalog").is_empty());
    let conn = db.guard().await.expect("SQLite guard");
    let mut rows = conn
        .query(
            "SELECT count(*) FROM _migrations WHERE checksum IS NULL",
            (),
        )
        .await
        .expect("check ledger");
    let count: i64 = rows
        .next()
        .await
        .expect("read result")
        .expect("count row")
        .get(0)
        .expect("count");
    assert_eq!(count, 0);
}
