//! Disposable PostgreSQL laboratory for an unregistered ingress guard candidate.
//!
//! This intentionally does not call production startup or authorize migration
//! installation. Each test creates its own database and login roles so it can
//! revoke PUBLIC's TEMP privilege without changing the shared test database.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};

const CANDIDATE: &str = include_str!("fixtures/ingress_epoch_relation_guard_candidate.sql");
const TABLES: [&str; 12] = [
    "ingress_archive_dispatch",
    "ingress_carbon_receipts",
    "ingress_deliveries",
    "ingress_delivery_receipts",
    "ingress_effect_descendants",
    "ingress_effect_intents",
    "ingress_effect_receipts",
    "ingress_messages",
    "ingress_origin_aliases",
    "ingress_send_attempts",
    "ingress_sm_refs",
    "ingress_sm_streams",
];

struct Fixture {
    admin: PgPool,
    owner_options: PgConnectOptions,
    runtime_options: PgConnectOptions,
    admin_options: PgConnectOptions,
    database: String,
    owner: String,
    migrator: String,
    runtime: String,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let url = waddle_ws_test_support::require_test_postgres_url()?;
        let admin = PgPool::connect(&url)
            .await
            .expect("connect test administrator");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("guard_db_{suffix}");
        let owner = format!("guard_owner_{suffix}");
        let migrator = format!("guard_migrator_{suffix}");
        let runtime = format!("guard_runtime_{suffix}");
        let password = format!("test_{}", uuid::Uuid::new_v4().simple());
        for statement in [
            format!("CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
            format!("CREATE ROLE {migrator} LOGIN PASSWORD '{password}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
            format!("CREATE ROLE {runtime} LOGIN PASSWORD '{password}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
            format!("GRANT {owner} TO {migrator}"),
            format!("CREATE DATABASE {database} OWNER {owner}"),
            format!("REVOKE CONNECT, TEMPORARY ON DATABASE {database} FROM PUBLIC"),
            format!("GRANT CONNECT ON DATABASE {database} TO {migrator}, {runtime}"),
        ] {
            sqlx::query(&statement).execute(&admin).await.expect("provision isolated test role/database");
        }
        let base = PgConnectOptions::from_str(&url).expect("parse admin options");
        let admin_options = base.clone().database(&database);
        let owner_options = base
            .clone()
            .database(&database)
            .username(&migrator)
            .password(&password)
            .options([("role", owner.as_str()), ("search_path", "guarded")]);
        let runtime_options = base
            .database(&database)
            .username(&runtime)
            .password(&password)
            .options([("search_path", "guarded")]);
        let mut db_admin = PgConnection::connect_with(&admin_options)
            .await
            .expect("connect isolated database");
        sqlx::query(&format!("CREATE SCHEMA guarded AUTHORIZATION {owner}"))
            .execute(&mut db_admin)
            .await
            .expect("create owner schema");
        sqlx::query("REVOKE ALL ON SCHEMA public FROM PUBLIC")
            .execute(&mut db_admin)
            .await
            .expect("revoke public schema grants");
        sqlx::query(&format!("GRANT USAGE ON SCHEMA guarded TO {runtime}"))
            .execute(&mut db_admin)
            .await
            .expect("grant runtime schema usage");
        drop(db_admin);

        let migration_url = migration_url(&url, &database, &migrator, &password, &owner);
        let db = Database::from_config(
            "guard-lab",
            &DatabaseConfig::new(DatabaseDriver::Postgres, migration_url),
        )
        .await
        .expect("open owner migration pool");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("apply actual complete migration catalog");
        drop(db);

        let mut owner_connection = PgConnection::connect_with(&owner_options)
            .await
            .expect("owner login");
        let last: i64 = sqlx::query_scalar("SELECT max(version) FROM guarded._migrations")
            .fetch_one(&mut owner_connection)
            .await
            .expect("migration version");
        assert_eq!(
            last, 1027,
            "laboratory must use the actual V1008-V1027 catalog"
        );
        sqlx::raw_sql(CANDIDATE)
            .execute(&mut owner_connection)
            .await
            .expect("install unregistered candidate");
        for table in TABLES {
            sqlx::query(&format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON guarded.{table} TO {runtime}"
            ))
            .execute(&mut owner_connection)
            .await
            .expect("grant guarded DML");
        }
        sqlx::query(&format!("GRANT SELECT ON guarded.ingress_protocol_epoch, guarded.ingress_epoch_guard_manifest TO {runtime}"))
            .execute(&mut owner_connection).await.expect("grant epoch and manifest SELECT");
        drop(owner_connection);
        Some(Self {
            admin,
            owner_options,
            runtime_options,
            admin_options,
            database,
            owner,
            migrator,
            runtime,
        })
    }

    async fn owner(&self) -> PgConnection {
        PgConnection::connect_with(&self.owner_options)
            .await
            .expect("connect owner login")
    }
    async fn runtime(&self) -> PgConnection {
        PgConnection::connect_with(&self.runtime_options)
            .await
            .expect("connect restricted runtime login")
    }
    async fn observer(&self) -> PgConnection {
        PgConnection::connect_with(&self.admin_options)
            .await
            .expect("connect observer")
    }
    async fn cleanup(self) {
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.admin)
            .await
            .expect("drop isolated database");
        for role in [&self.runtime, &self.migrator, &self.owner] {
            sqlx::query(&format!("DROP ROLE {role}"))
                .execute(&self.admin)
                .await
                .expect("drop isolated role");
        }
        self.admin.close().await;
    }
}

fn migration_url(url: &str, database: &str, migrator: &str, password: &str, owner: &str) -> String {
    let mut parsed = url::Url::parse(url).expect("parse test URL");
    let values: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "user" | "password" | "dbname" | "options"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    parsed
        .query_pairs_mut()
        .clear()
        .extend_pairs(values.iter().map(|(key, value)| (key, value)))
        .append_pair("dbname", database)
        .append_pair("user", migrator)
        .append_pair("password", password)
        .append_pair(
            "options",
            &format!("-c role={owner} -c search_path=guarded"),
        );
    parsed.to_string()
}

async fn begin(connection: &mut PgConnection) {
    sqlx::query("BEGIN ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *connection)
        .await
        .expect("begin READ COMMITTED");
    sqlx::query("SET LOCAL lock_timeout = '10s'")
        .execute(connection)
        .await
        .expect("bound fixture lock waits");
}

async fn owner_lock(connection: &mut PgConnection) {
    sqlx::query("LOCK TABLE ONLY guarded.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE")
        .execute(connection)
        .await
        .expect("owner locks epoch first");
}

async fn writer_lock(connection: &mut PgConnection) {
    sqlx::query("LOCK TABLE ONLY guarded.ingress_protocol_epoch IN ACCESS SHARE MODE")
        .execute(connection)
        .await
        .expect("writer prelocks actual epoch relation");
}

async fn advance(connection: &mut PgConnection) {
    sqlx::query("UPDATE guarded.ingress_protocol_epoch SET epoch = epoch + 1, activated_at = now(), lineage_uuid = '00000000-0000-0000-0000-000000000001' WHERE id = 1")
        .execute(connection).await.expect("advance epoch");
}

async fn commit(connection: &mut PgConnection) {
    sqlx::query("COMMIT")
        .execute(connection)
        .await
        .expect("commit");
}

async fn proof(connection: &mut PgConnection, epoch: i64) {
    sqlx::query("SELECT set_config('waddle.protocol_epoch', $1, true), set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)")
        .bind(epoch.to_string()).execute(connection).await.expect("install transaction proof");
}

async fn first_column(connection: &mut PgConnection, table: &str) -> String {
    sqlx::query_scalar("SELECT a.attname::text FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid = a.attrelid JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'guarded' AND c.relname = $1 AND a.attnum > 0 AND NOT a.attisdropped AND a.attgenerated = '' AND a.attidentity = '' ORDER BY a.attnum LIMIT 1")
        .bind(table).fetch_one(connection).await.expect("ordinary table column")
}

fn zero_dml(table: &str, column: &str) -> [String; 3] {
    [
        format!("INSERT INTO guarded.{table} ({column}) SELECT {column} FROM guarded.{table} WHERE false"),
        format!("UPDATE guarded.{table} SET {column} = {column} WHERE false"),
        format!("DELETE FROM guarded.{table} WHERE false"),
    ]
}

async fn check_proofs(connection: &mut PgConnection, statement: &str, epoch: i64) {
    begin(connection).await;
    writer_lock(connection).await;
    let error = sqlx::query(statement)
        .execute(&mut *connection)
        .await
        .expect_err("missing epoch proof must fail");
    assert!(
        error.to_string().contains("transaction-local epoch proof"),
        "{statement}: {error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .expect("rollback missing proof");

    begin(connection).await;
    writer_lock(connection).await;
    proof(connection, if epoch == 0 { 1 } else { 0 }).await;
    let error = sqlx::query(statement)
        .execute(&mut *connection)
        .await
        .expect_err("wrong epoch proof must fail");
    assert!(
        error.to_string().contains("transaction-local epoch proof"),
        "{statement}: {error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .expect("rollback wrong proof");

    begin(connection).await;
    writer_lock(connection).await;
    sqlx::query("SELECT set_config('waddle.protocol_epoch', $1, true), set_config('waddle.protocol_epoch_xid', '0', true)")
        .bind(epoch.to_string()).execute(&mut *connection).await.expect("install wrong transaction proof");
    let error = sqlx::query(statement)
        .execute(&mut *connection)
        .await
        .expect_err("wrong transaction proof must fail");
    assert!(
        error.to_string().contains("transaction-local epoch proof"),
        "{statement}: {error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .expect("rollback wrong transaction proof");

    begin(connection).await;
    writer_lock(connection).await;
    proof(connection, epoch).await;
    sqlx::query(statement)
        .execute(&mut *connection)
        .await
        .expect("current proof must pass");
    commit(connection).await;
}

async fn pid(connection: &mut PgConnection) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(connection)
        .await
        .expect("backend pid")
}

async fn wait_blocked(observer: &mut PgConnection, waiter: i32, blocker: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool =
                sqlx::query_scalar("SELECT $1 = ANY(pg_catalog.pg_blocking_pids($2))")
                    .bind(blocker)
                    .bind(waiter)
                    .fetch_one(&mut *observer)
                    .await
                    .expect("observe blockers");
            if blocked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("expected backend did not block on the identified holder");
}

#[tokio::test]
async fn candidate_guards_all_tables_with_select_only_epoch_login() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut runtime = fixture.runtime().await;
    let mut owner = fixture.owner().await;
    let manifest: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM guarded.ingress_epoch_guard_manifest ORDER BY table_name",
    )
    .fetch_all(&mut runtime)
    .await
    .expect("read actual manifest");
    assert_eq!(manifest, TABLES);
    let role_state: (String, String, bool, bool, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT session_user::text, current_user::text, pg_catalog.has_database_privilege(current_user, current_database(), 'TEMP'), pg_catalog.has_schema_privilege(current_user, 'guarded', 'CREATE'), pg_catalog.has_table_privilege(current_user, 'guarded.ingress_protocol_epoch', 'UPDATE'), pg_catalog.pg_has_role(current_user, $1, 'MEMBER'), pg_catalog.pg_has_role(current_user, $2, 'MEMBER'), current_user = pg_catalog.pg_get_userbyid((SELECT datdba FROM pg_catalog.pg_database WHERE datname = current_database()))"
    ).bind(&fixture.owner).bind(&fixture.migrator).fetch_one(&mut runtime).await.expect("inspect effective runtime privileges");
    assert_eq!(role_state.0, fixture.runtime);
    assert_eq!(role_state.1, fixture.runtime);
    assert!(
        !role_state.2
            && !role_state.3
            && !role_state.4
            && !role_state.5
            && !role_state.6
            && !role_state.7
    );
    let forbidden: (bool, bool, bool, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT pg_catalog.has_any_column_privilege(current_user, 'guarded.ingress_protocol_epoch', 'UPDATE'), pg_catalog.pg_has_role(current_user, $1, 'SET'), pg_catalog.pg_has_role(current_user, $2, 'SET'), current_user = pg_catalog.pg_get_userbyid((SELECT nspowner FROM pg_catalog.pg_namespace WHERE nspname = 'guarded')), current_user = pg_catalog.pg_get_userbyid((SELECT proowner FROM pg_catalog.pg_proc WHERE oid = 'guarded.waddle_ingress_epoch_guard()'::regprocedure)), pg_catalog.has_database_privilege(current_user, current_database(), 'CREATE'), (SELECT rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls FROM pg_catalog.pg_roles WHERE rolname = current_user)"
    ).bind(&fixture.owner).bind(&fixture.migrator).fetch_one(&mut runtime).await.expect("inspect column, role and object ownership");
    assert!(
        !forbidden.0
            && !forbidden.1
            && !forbidden.2
            && !forbidden.3
            && !forbidden.4
            && !forbidden.5
            && !forbidden.6
    );
    assert!(sqlx::query("CREATE TEMP TABLE cannot_create_temp (id int)")
        .execute(&mut runtime)
        .await
        .is_err());
    assert!(sqlx::query(&format!("SET ROLE {}", fixture.owner))
        .execute(&mut runtime)
        .await
        .is_err());
    assert!(sqlx::query(&format!("SET ROLE {}", fixture.migrator))
        .execute(&mut runtime)
        .await
        .is_err());
    assert!(
        sqlx::query("UPDATE guarded.ingress_protocol_epoch SET epoch = epoch + 1")
            .execute(&mut runtime)
            .await
            .is_err()
    );
    assert!(sqlx::query("CREATE TABLE guarded.runtime_owned (id int)")
        .execute(&mut runtime)
        .await
        .is_err());
    assert!(
        sqlx::query("DELETE FROM guarded.ingress_epoch_guard_manifest")
            .execute(&mut runtime)
            .await
            .is_err()
    );
    for table in ["ingress_protocol_epoch", "ingress_epoch_guard_manifest"] {
        let object_owner: String = sqlx::query_scalar("SELECT pg_catalog.pg_get_userbyid(c.relowner) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'guarded' AND c.relname = $1")
            .bind(table).fetch_one(&mut runtime).await.expect("authority table owner");
        assert_eq!(object_owner, fixture.owner);
    }
    for table in TABLES {
        let owner_name: String = sqlx::query_scalar("SELECT pg_catalog.pg_get_userbyid(c.relowner) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'guarded' AND c.relname = $1")
            .bind(table).fetch_one(&mut runtime).await.expect("guarded table owner");
        assert_eq!(owner_name, fixture.owner);
        let always_guards: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_trigger t WHERE t.tgrelid = pg_catalog.to_regclass(pg_catalog.format('guarded.%I', $1)) AND NOT t.tgisinternal AND t.tgenabled = 'A' AND ((t.tgname = $1 || '_epoch_guard_dml' AND t.tgfoid = 'guarded.waddle_ingress_epoch_guard()'::regprocedure AND t.tgtype = 30) OR (t.tgname = $1 || '_epoch_guard_truncate' AND t.tgfoid = 'guarded.waddle_ingress_truncate_guard()'::regprocedure AND t.tgtype = 34))")
            .bind(table).fetch_one(&mut runtime).await.expect("inspect enabled guard triggers");
        assert_eq!(
            always_guards, 2,
            "{table} must retain both ENABLE ALWAYS guards"
        );
        let column = first_column(&mut runtime, table).await;
        for statement in zero_dml(table, &column) {
            let error = sqlx::query(&statement)
                .execute(&mut runtime)
                .await
                .expect_err("raw DML without prelock must fail");
            assert!(
                error
                    .to_string()
                    .contains("require an epoch ACCESS SHARE prelock"),
                "{table}: {error}"
            );
            begin(&mut runtime).await;
            proof(&mut runtime, 0).await;
            let error = sqlx::query(&statement)
                .execute(&mut runtime)
                .await
                .expect_err("valid GUCs must not substitute for prelock");
            assert!(
                error
                    .to_string()
                    .contains("require an epoch ACCESS SHARE prelock"),
                "{table}: {error}"
            );
            sqlx::query("ROLLBACK")
                .execute(&mut runtime)
                .await
                .expect("rollback raw writer");
            check_proofs(&mut runtime, &statement, 0).await;
        }
        assert!(
            sqlx::query(&format!("ALTER TABLE guarded.{table} DISABLE TRIGGER ALL"))
                .execute(&mut runtime)
                .await
                .is_err()
        );
        let error = sqlx::query(&format!("TRUNCATE guarded.{table} CASCADE"))
            .execute(&mut owner)
            .await
            .expect_err("owner TRUNCATE must be rejected by trigger");
        assert!(
            error.to_string().contains("TRUNCATE is not permitted"),
            "{table}: {error}"
        );
    }
    begin(&mut owner).await;
    owner_lock(&mut owner).await;
    advance(&mut owner).await;
    commit(&mut owner).await;
    for table in TABLES {
        let column = first_column(&mut runtime, table).await;
        for statement in zero_dml(table, &column) {
            check_proofs(&mut runtime, &statement, 1).await;
        }
    }
    drop(runtime);
    drop(owner);
    fixture.cleanup().await;
}

#[tokio::test]
async fn candidate_requires_ax_first_and_rejects_duplicate_advance() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut owner = fixture.owner().await;
    let mut runtime = fixture.runtime().await;
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let error = tokio::time::timeout(Duration::from_secs(3), sqlx::query("UPDATE guarded.ingress_protocol_epoch SET epoch = epoch + 1, activated_at = now(), lineage_uuid = '00000000-0000-0000-0000-000000000001' WHERE id = 1")
        .execute(&mut owner)).await.expect("direct update must fail without waiting for writer").expect_err("legacy direct update must fail");
    assert!(
        error
            .to_string()
            .contains("requires ACCESS EXCLUSIVE first"),
        "{error}"
    );
    commit(&mut runtime).await;
    begin(&mut owner).await;
    owner_lock(&mut owner).await;
    advance(&mut owner).await;
    let error =
        sqlx::query("UPDATE guarded.ingress_protocol_epoch SET epoch = epoch + 1 WHERE id = 1")
            .execute(&mut owner)
            .await
            .expect_err("two advances in one transaction must fail");
    assert!(
        error
            .to_string()
            .contains("only one ingress protocol epoch advance"),
        "{error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut owner)
        .await
        .expect("rollback duplicate transition");
    let epoch: i64 =
        sqlx::query_scalar("SELECT epoch FROM guarded.ingress_protocol_epoch WHERE id = 1")
            .fetch_one(&mut owner)
            .await
            .expect("read epoch");
    assert_eq!(epoch, 0);
    drop((owner, runtime));
    fixture.cleanup().await;
}

#[tokio::test]
async fn candidate_raw_dml_refuses_without_waiting_for_owner_epoch_lock() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut owner = fixture.owner().await;
    let mut runtime = fixture.runtime().await;
    begin(&mut owner).await;
    owner_lock(&mut owner).await;
    begin(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        sqlx::query("DELETE FROM guarded.ingress_messages WHERE false").execute(&mut runtime),
    )
    .await
    .expect("raw DML waited behind owner epoch lock")
    .expect_err("raw DML must fail without prelock");
    assert!(
        error
            .to_string()
            .contains("require an epoch ACCESS SHARE prelock"),
        "{error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback raw DML");
    commit(&mut owner).await;
    drop((owner, runtime));
    fixture.cleanup().await;
}

#[tokio::test]
async fn candidate_waits_for_transition_and_reads_fresh_epoch() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut owner = fixture.owner().await;
    let mut runtime = fixture.runtime().await;
    let mut observer = fixture.observer().await;
    begin(&mut owner).await;
    let owner_pid = pid(&mut owner).await;
    owner_lock(&mut owner).await;
    advance(&mut owner).await;
    begin(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let runtime_pid = pid(&mut runtime).await;
    let waiter = tokio::spawn(async move {
        writer_lock(&mut runtime).await;
        let result = sqlx::query("DELETE FROM guarded.ingress_messages WHERE false")
            .execute(&mut runtime)
            .await;
        (runtime, result)
    });
    wait_blocked(&mut observer, runtime_pid, owner_pid).await;
    commit(&mut owner).await;
    let (mut runtime, result) = waiter.await.expect("join blocked writer");
    let error = result.expect_err("stale epoch proof must not cross transition");
    assert!(
        error.to_string().contains("transaction-local epoch proof"),
        "{error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback stale writer");
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 1).await;
    sqlx::query("DELETE FROM guarded.ingress_messages WHERE false")
        .execute(&mut runtime)
        .await
        .expect("new epoch proof succeeds");
    commit(&mut runtime).await;
    begin(&mut owner).await;
    owner_lock(&mut owner).await;
    advance(&mut owner).await;
    begin(&mut runtime).await;
    let runtime_pid = pid(&mut runtime).await;
    let waiter = tokio::spawn(async move {
        writer_lock(&mut runtime).await;
        let epoch: i64 =
            sqlx::query_scalar("SELECT epoch FROM guarded.ingress_protocol_epoch WHERE id = 1")
                .fetch_one(&mut runtime)
                .await
                .expect("read epoch after barrier");
        proof(&mut runtime, epoch).await;
        sqlx::query("DELETE FROM guarded.ingress_messages WHERE false")
            .execute(&mut runtime)
            .await
            .expect("fresh post-wait write");
        commit(&mut runtime).await;
        (runtime, epoch)
    });
    wait_blocked(&mut observer, runtime_pid, owner_pid).await;
    commit(&mut owner).await;
    let (runtime, observed) = waiter.await.expect("join fresh writer");
    assert_eq!(observed, 2);
    drop((owner, runtime, observer));
    fixture.cleanup().await;
}

#[tokio::test]
async fn candidate_transition_waits_for_two_independent_writers() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut first = fixture.runtime().await;
    let mut second = fixture.runtime().await;
    let mut owner = fixture.owner().await;
    let mut observer = fixture.observer().await;
    begin(&mut first).await;
    begin(&mut second).await;
    let first_pid = pid(&mut first).await;
    let second_pid = pid(&mut second).await;
    for writer in [&mut first, &mut second] {
        writer_lock(&mut *writer).await;
        proof(&mut *writer, 0).await;
        sqlx::query("DELETE FROM guarded.ingress_messages WHERE false")
            .execute(&mut *writer)
            .await
            .expect("independent guarded writer");
        let row_share: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid = pg_catalog.pg_backend_pid() AND relation = 'guarded.ingress_protocol_epoch'::regclass AND mode = 'RowShareLock' AND granted)")
            .fetch_one(&mut *writer).await.expect("inspect epoch locks");
        assert!(
            !row_share,
            "candidate must not acquire a global row-share lock"
        );
    }
    begin(&mut owner).await;
    let owner_pid = pid(&mut owner).await;
    let waiter = tokio::spawn(async move {
        owner_lock(&mut owner).await;
        owner
    });
    wait_blocked(&mut observer, owner_pid, first_pid).await;
    wait_blocked(&mut observer, owner_pid, second_pid).await;
    commit(&mut first).await;
    wait_blocked(&mut observer, owner_pid, second_pid).await;
    commit(&mut second).await;
    let mut owner = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("transition timed out")
        .expect("join transition");
    advance(&mut owner).await;
    commit(&mut owner).await;
    drop((first, second, owner, observer));
    fixture.cleanup().await;
}

#[tokio::test]
async fn candidate_rejects_pinned_repeatable_read_and_stale_savepoint_proof() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut owner = fixture.owner().await;
    let mut runtime = fixture.runtime().await;
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut runtime)
        .await
        .expect("begin RR");
    let _: i64 = sqlx::query_scalar("SELECT count(*) FROM guarded.ingress_epoch_guard_manifest")
        .fetch_one(&mut runtime)
        .await
        .expect("pin snapshot without locking epoch");
    begin(&mut owner).await;
    owner_lock(&mut owner).await;
    advance(&mut owner).await;
    commit(&mut owner).await;
    let epoch: i64 =
        sqlx::query_scalar("SELECT epoch FROM guarded.ingress_protocol_epoch WHERE id = 1")
            .fetch_one(&mut runtime)
            .await
            .expect("read pinned epoch");
    assert_eq!(epoch, 0, "the pinned snapshot must remain stale");
    let error = sqlx::query("DELETE FROM guarded.ingress_messages WHERE false")
        .execute(&mut runtime)
        .await
        .expect_err("RR guarded write must fail");
    assert!(
        error.to_string().contains("require READ COMMITTED"),
        "{error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback RR");
    begin(&mut runtime).await;
    proof(&mut runtime, 1).await;
    sqlx::query("SAVEPOINT before_guard")
        .execute(&mut runtime)
        .await
        .expect("savepoint");
    writer_lock(&mut runtime).await;
    sqlx::query("DELETE FROM guarded.ingress_messages WHERE false")
        .execute(&mut runtime)
        .await
        .expect("guarded write in savepoint");
    sqlx::query("ROLLBACK TO SAVEPOINT before_guard")
        .execute(&mut runtime)
        .await
        .expect("rollback guarded savepoint");
    begin(&mut owner).await;
    owner_lock(&mut owner).await;
    advance(&mut owner).await;
    commit(&mut owner).await;
    writer_lock(&mut runtime).await;
    let error = sqlx::query("DELETE FROM guarded.ingress_messages WHERE false")
        .execute(&mut runtime)
        .await
        .expect_err("stale retained proof must fail");
    assert!(
        error.to_string().contains("transaction-local epoch proof"),
        "{error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback stale proof");
    drop((owner, runtime));
    fixture.cleanup().await;
}

#[tokio::test]
async fn candidate_temp_shadowing_requires_drain_and_qualified_writes() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut admin = fixture.observer().await;
    let mut owner = fixture.owner().await;
    begin(&mut owner).await;
    owner_lock(&mut owner).await;
    advance(&mut owner).await;
    commit(&mut owner).await;
    sqlx::query(&format!(
        "GRANT TEMPORARY ON DATABASE {} TO {}",
        fixture.database, fixture.runtime
    ))
    .execute(&mut admin)
    .await
    .expect("simulate old TEMP grant");
    let mut runtime = fixture.runtime().await;
    for table in TABLES {
        sqlx::query(&format!(
            "CREATE TEMP TABLE {table} AS TABLE guarded.{table} WITH NO DATA"
        ))
        .execute(&mut runtime)
        .await
        .expect("create shadow of guarded table");
        begin(&mut runtime).await;
        writer_lock(&mut runtime).await;
        proof(&mut runtime, 0).await;
        sqlx::query(&format!("DELETE FROM {table} WHERE false"))
            .execute(&mut runtime)
            .await
            .expect("unqualified statement bypasses real guard despite an epoch prelock");
        let error = sqlx::query(&format!("DELETE FROM guarded.{table} WHERE false"))
            .execute(&mut runtime)
            .await
            .expect_err("qualified statement reaches real guard");
        assert!(
            error.to_string().contains("transaction-local epoch proof"),
            "{table}: {error}"
        );
        sqlx::query("ROLLBACK")
            .execute(&mut runtime)
            .await
            .expect("rollback missing proof");
    }
    sqlx::query("CREATE TEMP TABLE ingress_protocol_epoch (id int, epoch bigint)")
        .execute(&mut runtime)
        .await
        .expect("shadow epoch");
    sqlx::query("INSERT INTO ingress_protocol_epoch VALUES (1, 0)")
        .execute(&mut runtime)
        .await
        .expect("forge epoch zero");
    begin(&mut runtime).await;
    sqlx::query("LOCK TABLE ONLY pg_temp.ingress_protocol_epoch IN ACCESS SHARE MODE")
        .execute(&mut runtime)
        .await
        .expect("prelock forged TEMP epoch");
    proof(&mut runtime, 1).await;
    let error = sqlx::query("DELETE FROM guarded.ingress_messages WHERE false")
        .execute(&mut runtime)
        .await
        .expect_err("TEMP epoch lock cannot impersonate real prelock");
    assert!(
        error
            .to_string()
            .contains("require an epoch ACCESS SHARE prelock"),
        "{error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback TEMP prelock attempt");
    begin(&mut runtime).await;
    sqlx::query("SET LOCAL search_path = pg_temp, guarded")
        .execute(&mut runtime)
        .await
        .expect("prefer the TEMP schema");
    let forged: i64 = sqlx::query_scalar("SELECT epoch FROM ingress_protocol_epoch WHERE id = 1")
        .fetch_one(&mut runtime)
        .await
        .expect("read forged unqualified epoch");
    assert_eq!(forged, 0);
    writer_lock(&mut runtime).await;
    proof(&mut runtime, forged).await;
    let error = sqlx::query("DELETE FROM guarded.ingress_messages WHERE false")
        .execute(&mut runtime)
        .await
        .expect_err("real epoch must remain authoritative");
    assert!(
        error.to_string().contains("transaction-local epoch proof"),
        "{error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback spoof attempt");
    sqlx::query(&format!(
        "REVOKE TEMPORARY ON DATABASE {} FROM {}",
        fixture.database, fixture.runtime
    ))
    .execute(&mut admin)
    .await
    .expect("revoke old TEMP grant");
    let has_temp: bool = sqlx::query_scalar(
        "SELECT pg_catalog.has_database_privilege(current_user, current_database(), 'TEMP')",
    )
    .fetch_one(&mut runtime)
    .await
    .expect("effective TEMP privilege");
    assert!(!has_temp);
    // Revocation does not remove temp relations already created in this session.
    sqlx::query("DELETE FROM ingress_messages WHERE false")
        .execute(&mut runtime)
        .await
        .expect("old session still resolves its TEMP shadow");
    drop(runtime);
    let mut fresh = fixture.runtime().await;
    assert!(
        sqlx::query("CREATE TEMP TABLE denied_after_reconnect (id int)")
            .execute(&mut fresh)
            .await
            .is_err()
    );
    begin(&mut fresh).await;
    writer_lock(&mut fresh).await;
    let error = sqlx::query("DELETE FROM ingress_messages WHERE false")
        .execute(&mut fresh)
        .await
        .expect_err("fresh unqualified write resolves to guarded table");
    assert!(
        error.to_string().contains("transaction-local epoch proof"),
        "{error}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut fresh)
        .await
        .expect("rollback missing proof");
    drop((fresh, owner, admin));
    fixture.cleanup().await;
}
