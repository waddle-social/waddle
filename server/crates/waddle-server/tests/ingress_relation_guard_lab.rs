//! Disposable PostgreSQL laboratory for an unregistered ingress guard candidate.
//!
//! This intentionally does not call production startup or authorize migration
//! installation. Each test creates its own database and login roles so it can
//! revoke PUBLIC's TEMP privilege without changing the shared test database.

use std::str::FromStr;
use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection, Row as _};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};

const CANDIDATE: &str =
    include_str!("../src/db/migrations/owner/ingress_epoch_relation_guard_candidate.sql");
const DEFINER_API_CANDIDATE: &str =
    include_str!("../src/db/migrations/owner/ingress_definer_api_candidate.sql");
const TABLES: [&str; 13] = [
    "ingress_archive_dispatch",
    "ingress_carbon_receipts",
    "ingress_deliveries",
    "ingress_delivery_receipts",
    "ingress_effect_descendants",
    "ingress_effect_intents",
    "ingress_effect_receipts",
    "ingress_messages",
    "ingress_origin_aliases",
    "ingress_owner_rejections",
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
    extra_roles: Vec<String>,
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
            last, 1028,
            "laboratory must use the actual V1008-V1028 catalog"
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
            extra_roles: Vec::new(),
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
        let mut roles = vec![self.runtime, self.migrator, self.owner];
        roles.extend(self.extra_roles);
        for role in roles {
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
    sqlx::query("SELECT pg_catalog.set_config('waddle.protocol_epoch', $1, true), pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::pg_catalog.text, true)")
        .bind(epoch.to_string()).execute(connection).await.expect("install transaction proof");
}

async fn forged_proof(connection: &mut PgConnection) {
    sqlx::query(
        "SELECT pg_catalog.set_config('waddle.protocol_epoch', '999', true), \
                pg_catalog.set_config('waddle.protocol_epoch_xid', 'forged', true)",
    )
    .execute(connection)
    .await
    .expect("install forged transaction proof");
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

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn render_definer_api_candidate(schema: &str, api_owner: &str, runtime: &str) -> String {
    let schema = quote_identifier(schema);
    let api_owner_ident = quote_identifier(api_owner);
    let runtime_ident = quote_identifier(runtime);
    DEFINER_API_CANDIDATE
        .replace("__SCHEMA_PATH__", &schema)
        .replace("__SCHEMA__", &schema)
        .replace("__API_OWNER_LITERAL__", &api_owner.replace('\'', "''"))
        .replace("__RUNTIME_ROLE_LITERAL__", &runtime.replace('\'', "''"))
        .replace("__API_OWNER_IDENT__", &api_owner_ident)
        .replace("__RUNTIME_ROLE_IDENT__", &runtime_ident)
}

fn alias_hash(sender: &str, target_jid: &str, target_kind: i32, origin_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"waddle:ingress-alias-key:v1\0");
    for part in [sender, target_jid, origin_id] {
        hasher.update(
            u32::try_from(part.len())
                .expect("fixture alias field length")
                .to_be_bytes(),
        );
        hasher.update(part.as_bytes());
    }
    hasher.update(target_kind.to_be_bytes());
    hasher.finalize().into()
}

struct OriginRequest<'a> {
    message_key: uuid::Uuid,
    hash: [u8; 32],
    sender: &'a str,
    target_jid: &'a str,
    origin_id: &'a str,
    digest: [u8; 32],
    source_context: Option<uuid::Uuid>,
    source_sm_ingress_id: Option<uuid::Uuid>,
}

async fn resolve_origin(
    connection: &mut PgConnection,
    request: OriginRequest<'_>,
) -> (Option<uuid::Uuid>, String, Option<i32>, Option<Vec<u8>>) {
    let OriginRequest {
        message_key,
        hash,
        sender,
        target_jid,
        origin_id,
        digest,
        source_context,
        source_sm_ingress_id,
    } = request;
    let source_bare = source_context.map(|_| sender.to_owned());
    let source_context_version = source_context.map(|_| 1_i64);
    let source_auth_epoch = source_context.map(|_| 1_i64);
    let source_ingress_ordinal = source_sm_ingress_id.map(|_| 1_i64);
    let row = sqlx::query(
        "SELECT canonical_message_key, outcome, stored_digest_version, stored_digest \
         FROM guarded.waddle_ingress_resolve_origin(\
           $1::pg_catalog.uuid, $2::pg_catalog.int4, $3::pg_catalog.bytea, \
           $4::pg_catalog.bytea, $5::pg_catalog.text, $6::pg_catalog.int4, \
           $7::pg_catalog.text, $8::pg_catalog.text, $9::pg_catalog.text, \
           $10::pg_catalog.uuid, $11::pg_catalog.int8, $12::pg_catalog.int8, \
           $13::pg_catalog.uuid, $14::pg_catalog.numeric)",
    )
    .bind(message_key)
    .bind(1_i32)
    .bind(digest.to_vec())
    .bind(hash.to_vec())
    .bind(sender)
    .bind(1_i32)
    .bind(target_jid)
    .bind(origin_id)
    .bind(source_bare)
    .bind(source_context)
    .bind(source_context_version)
    .bind(source_auth_epoch)
    .bind(source_sm_ingress_id)
    .bind(source_ingress_ordinal)
    .fetch_one(connection)
    .await
    .expect("invoke operation-specific definer function");
    (
        row.try_get(0).expect("canonical message key"),
        row.try_get(1).expect("operation outcome"),
        row.try_get(2).expect("stored digest version"),
        row.try_get(3).expect("stored digest"),
    )
}

async fn assert_no_guarded_dml(connection: &mut PgConnection, runtime: &str) {
    for table in TABLES {
        let relation = format!(
            "{}.{}",
            quote_identifier("guarded"),
            quote_identifier(table)
        );
        let can_write: bool = sqlx::query_scalar(
            "SELECT pg_catalog.has_table_privilege($1, $2, 'INSERT') \
               OR pg_catalog.has_table_privilege($1, $2, 'UPDATE') \
               OR pg_catalog.has_table_privilege($1, $2, 'DELETE') \
               OR pg_catalog.has_table_privilege($1, $2, 'TRUNCATE')",
        )
        .bind(runtime)
        .bind(relation)
        .fetch_one(&mut *connection)
        .await
        .expect("inspect guarded DML privileges");
        assert!(!can_write, "runtime unexpectedly has DML on {table}");
    }
}

#[tokio::test]
async fn definer_api_keeps_canonical_resolution_inside_outer_transaction() {
    let Some(mut fixture) = Fixture::new().await else {
        return;
    };
    let mut version_check = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("connect disposable PostgreSQL database");
    let server_version: i32 =
        sqlx::query_scalar("SELECT pg_catalog.current_setting('server_version_num')::integer")
            .fetch_one(&mut version_check)
            .await
            .expect("read PostgreSQL version");
    drop(version_check);
    if server_version < 170_000 {
        eprintln!("skipping definer API pilot: requires PostgreSQL 17, found {server_version}");
        fixture.cleanup().await;
        return;
    }

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let api_owner = format!("guard_api_{suffix}");
    fixture.extra_roles.push(api_owner.clone());
    sqlx::query(&format!(
        "CREATE ROLE {api_owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT"
    ))
    .execute(&fixture.admin)
    .await
    .expect("create dedicated API owner");

    let sender = format!("definer{suffix}@example.test");
    let username = format!("definer{suffix}");
    let context_a = uuid::Uuid::new_v4();
    let context_b = uuid::Uuid::new_v4();
    let expired_context = uuid::Uuid::new_v4();
    let mut owner = fixture.owner().await;
    sqlx::query(
        "INSERT INTO guarded.users \
         (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) \
         VALUES ($1, $2, $2, $2, '2026-11-05T00:00:00Z', '2026-11-05T00:00:00Z')",
    )
    .bind(&sender)
    .bind(&username)
    .execute(&mut owner)
    .await
    .expect("seed source principal user");
    for (index, context) in [context_a, context_b].into_iter().enumerate() {
        sqlx::query(
            "INSERT INTO guarded.sessions \
             (id, user_jid, token_hash, auth_context_id, auth_context_version, \
              principal_auth_epoch, created_at, last_used_at) \
             VALUES ($1, $2, $3, $4, 1, 1, '2026-11-05T00:00:00Z', '2026-11-05T00:00:00Z')",
        )
        .bind(format!("session-{index}-{suffix}"))
        .bind(&sender)
        .bind(format!("token-{index}-{suffix}"))
        .bind(context.to_string())
        .execute(&mut owner)
        .await
        .expect("seed valid principal session");
    }
    sqlx::query(
        "INSERT INTO guarded.sessions \
         (id, user_jid, token_hash, auth_context_id, auth_context_version, \
          principal_auth_epoch, created_at, last_used_at, expires_at) \
         VALUES ($1, $2, $3, $4, 1, 1, '2020-01-01T00:00:00Z', \
                 '2020-01-01T00:00:00Z', '2020-01-02T00:00:00Z')",
    )
    .bind(format!("expired-session-{suffix}"))
    .bind(&sender)
    .bind(format!("expired-token-{suffix}"))
    .bind(expired_context.to_string())
    .execute(&mut owner)
    .await
    .expect("seed expired principal session");

    // Preserve a pre-boundary legacy alias with no source tuple. Its replay
    // remains valid after the definer API is installed.
    let legacy_origin = format!("legacy-{suffix}");
    let legacy_hash = alias_hash(&sender, "target@example.test", 1, &legacy_origin);
    let legacy_key = uuid::Uuid::new_v4();
    let retention_key = uuid::Uuid::new_v4();
    let retention_descendant = uuid::Uuid::new_v4();
    let retention_hash = [81_u8; 32];
    let legacy_digest = [34_u8; 32];
    begin(&mut owner).await;
    writer_lock(&mut owner).await;
    proof(&mut owner, 0).await;
    sqlx::query(
        "INSERT INTO guarded.ingress_messages (message_key, digest_version, digest) \
         VALUES ($1, 1, $2)",
    )
    .bind(legacy_key)
    .bind(legacy_digest.to_vec())
    .execute(&mut owner)
    .await
    .expect("seed legacy unbound message before API boundary");
    sqlx::query(
        "INSERT INTO guarded.ingress_origin_aliases \
         (alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key) \
         VALUES ($1, $2, 1, 'target@example.test', $3, $4)",
    )
    .bind(legacy_hash.to_vec())
    .bind(&sender)
    .bind(&legacy_origin)
    .bind(legacy_key)
    .execute(&mut owner)
    .await
    .expect("seed legacy unbound alias before API boundary");
    sqlx::query(
        "INSERT INTO guarded.ingress_messages \
         (message_key, digest_version, digest, terminal_at) \
         VALUES ($1, 1, decode(repeat('51', 32), 'hex'), '2000-01-01T00:00:00Z')",
    )
    .bind(retention_key)
    .execute(&mut owner)
    .await
    .expect("seed old terminal row for database-clock retention test");
    sqlx::query(
        "INSERT INTO guarded.ingress_effect_intents \
         (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) \
         VALUES ($1, 0, 1, $2, 1, '{}'::bytea)",
    )
    .bind(retention_key)
    .bind(retention_hash.to_vec())
    .execute(&mut owner)
    .await
    .expect("seed retention intent");
    sqlx::query(
        "INSERT INTO guarded.ingress_effect_receipts \
         (message_key, kind, semantic_identity_hash) VALUES ($1, 1, $2)",
    )
    .bind(retention_key)
    .bind(retention_hash.to_vec())
    .execute(&mut owner)
    .await
    .expect("seed completed retention intent");
    sqlx::query(
        "INSERT INTO guarded.ingress_effect_descendants \
         (message_key, kind, semantic_identity_hash, descendant_key) \
         VALUES ($1, 1, $2, $3)",
    )
    .bind(retention_key)
    .bind(retention_hash.to_vec())
    .bind(retention_descendant.to_string())
    .execute(&mut owner)
    .await
    .expect("seed unsettled retention descendant");
    commit(&mut owner).await;

    let candidate_sql = render_definer_api_candidate("guarded", &api_owner, &fixture.runtime);
    assert!(!candidate_sql.contains("__SCHEMA_"));
    assert!(!candidate_sql.contains("__API_OWNER_"));
    assert!(!candidate_sql.contains("__RUNTIME_ROLE_"));

    // Use the disposable test administrator only for the fixture's ownership
    // transfer; production provider-managed ownership and role provisioning
    // remain a separate rollout prerequisite.
    let mut transition_admin = fixture.observer().await;

    // Model the boundary change as one epoch-exclusive transaction: old
    // direct DML grants are removed as the two trigger functions are swapped.
    sqlx::query("BEGIN ISOLATION LEVEL READ COMMITTED")
        .execute(&mut transition_admin)
        .await
        .expect("begin isolated transition");
    sqlx::query("LOCK TABLE ONLY guarded.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE")
        .execute(&mut transition_admin)
        .await
        .expect("take epoch transition barrier first");
    for table in TABLES {
        sqlx::query(&format!(
            "REVOKE ALL PRIVILEGES ON TABLE guarded.{table} FROM {}",
            fixture.runtime
        ))
        .execute(&mut transition_admin)
        .await
        .expect("remove runtime table access");
    }
    sqlx::query(&format!(
        "GRANT SELECT ON guarded.ingress_protocol_epoch TO {}",
        fixture.runtime
    ))
    .execute(&mut transition_admin)
    .await
    .expect("allow runtime epoch prelock");
    for statement in [
        format!("GRANT USAGE, CREATE ON SCHEMA guarded TO {api_owner}"),
        format!("GRANT SELECT ON guarded.ingress_protocol_epoch TO {api_owner}"),
        format!("GRANT UPDATE (id) ON guarded.ingress_protocol_epoch TO {api_owner}"),
        format!("GRANT SELECT ON guarded.sessions TO {api_owner}"),
        format!("GRANT UPDATE (id) ON guarded.sessions TO {api_owner}"),
    ] {
        sqlx::query(&statement)
            .execute(&mut transition_admin)
            .await
            .expect("grant API owner its bounded privileges");
    }
    for table in TABLES {
        sqlx::query(&format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON guarded.{table} TO {api_owner}"
        ))
        .execute(&mut transition_admin)
        .await
        .expect("grant dedicated API owner guarded-table DML");
    }
    sqlx::raw_sql(&candidate_sql)
        .execute(&mut transition_admin)
        .await
        .expect("install dark guarded-table definer candidate");
    sqlx::query(&format!("REVOKE CREATE ON SCHEMA guarded FROM {api_owner}"))
        .execute(&mut transition_admin)
        .await
        .expect("remove API schema creation privilege");
    sqlx::query("COMMIT")
        .execute(&mut transition_admin)
        .await
        .expect("commit atomic guard and ACL transition");
    drop(transition_admin);

    assert_no_guarded_dml(&mut owner, &fixture.runtime).await;
    for table in TABLES {
        let table_owner: String = sqlx::query_scalar(
            "SELECT pg_catalog.pg_get_userbyid(c.relowner)::text \
             FROM pg_catalog.pg_class c \
             WHERE c.oid = pg_catalog.to_regclass($1)",
        )
        .bind(format!("guarded.{table}"))
        .fetch_one(&mut owner)
        .await
        .expect("inspect guarded table owner");
        assert_ne!(
            table_owner, api_owner,
            "API owner must not own guarded table {table}"
        );
    }
    let api_role_restricted: bool = sqlx::query_scalar(
        "SELECT NOT rolcanlogin AND NOT rolsuper AND NOT rolcreatedb AND \
                NOT rolcreaterole AND NOT rolreplication AND NOT rolbypassrls AND NOT rolinherit \
         FROM pg_catalog.pg_roles WHERE rolname = $1",
    )
    .bind(&api_owner)
    .fetch_one(&mut owner)
    .await
    .expect("inspect API owner role");
    assert!(api_role_restricted);
    let owner_is_function_owner: bool = sqlx::query_scalar(
        "SELECT r.rolname = $1 FROM pg_catalog.pg_proc p \
         JOIN pg_catalog.pg_roles r ON r.oid = p.proowner \
         WHERE p.oid = pg_catalog.to_regprocedure(\
           'guarded.waddle_ingress_resolve_origin(uuid,integer,bytea,bytea,text,integer,text,text,text,uuid,numeric,numeric,uuid,numeric)')",
    )
    .bind(&api_owner)
    .fetch_one(&mut owner)
    .await
    .expect("inspect definer function owner");
    assert!(owner_is_function_owner);
    let (security_definer, safe_search_path, runtime_execute): (bool, bool, bool) = sqlx::query_as(
        "SELECT p.prosecdef, \
           'search_path=pg_catalog, pg_temp' = ANY(p.proconfig), \
           pg_catalog.has_function_privilege($1, p.oid, 'EXECUTE') \
         FROM pg_catalog.pg_proc p \
         WHERE p.oid = pg_catalog.to_regprocedure(\
           'guarded.waddle_ingress_resolve_origin(uuid,integer,bytea,bytea,text,integer,text,text,text,uuid,numeric,numeric,uuid,numeric)')",
    )
    .bind(&fixture.runtime)
    .fetch_one(&mut owner)
    .await
    .expect("inspect definer security and execution ACL");
    assert!(security_definer);
    assert!(safe_search_path);
    assert!(runtime_execute);
    let (
        prepare_owner,
        prepare_definer,
        prepare_safe_path,
        runtime_can_prepare,
        public_can_prepare,
    ): (String, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT pg_catalog.pg_get_userbyid(p.proowner)::text, p.prosecdef, \
                'search_path=pg_catalog, pg_temp' = ANY(p.proconfig), \
                pg_catalog.has_function_privilege($1, p.oid, 'EXECUTE'), \
                EXISTS ( \
                    SELECT 1 FROM pg_catalog.aclexplode( \
                        COALESCE(p.proacl, pg_catalog.acldefault('f', p.proowner)) \
                    ) acl \
                    WHERE acl.grantee = 0 AND acl.privilege_type = 'EXECUTE' \
                ) \
         FROM pg_catalog.pg_proc p \
         WHERE p.oid = pg_catalog.to_regprocedure( \
           'guarded.waddle_ingress_definer_prepare()')",
    )
    .bind(&fixture.runtime)
    .fetch_one(&mut owner)
    .await
    .expect("inspect internal definer precondition helper");
    assert_eq!(prepare_owner, api_owner);
    assert!(
        !prepare_definer,
        "internal precondition must use invoker identity"
    );
    assert!(prepare_safe_path);
    assert!(
        !runtime_can_prepare,
        "runtime must not call the internal helper"
    );
    assert!(
        !public_can_prepare,
        "PUBLIC must not call the internal helper"
    );
    let (trigger_security_definer, trigger_safe_search_path): (bool, bool) = sqlx::query_as(
        "SELECT p.prosecdef, \
           'search_path=pg_catalog, pg_temp' = ANY(p.proconfig) \
         FROM pg_catalog.pg_proc p \
         WHERE p.oid = pg_catalog.to_regprocedure(\
           'guarded.waddle_ingress_definer_epoch_guard()')",
    )
    .fetch_one(&mut owner)
    .await
    .expect("inspect invoker trigger function path");
    assert!(!trigger_security_definer);
    assert!(trigger_safe_search_path);
    let retained_guard_safe_path: bool = sqlx::query_scalar(
        "SELECT 'search_path=pg_catalog, pg_temp' = ANY(p.proconfig) \
         FROM pg_catalog.pg_proc p \
         WHERE p.oid = pg_catalog.to_regprocedure( \
           'guarded.waddle_ingress_epoch_guard()')",
    )
    .fetch_one(&mut owner)
    .await
    .expect("inspect retained V1009 guard search path");
    assert!(retained_guard_safe_path);
    let timestamp_api_shape: (bool, bool, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT \
           pg_catalog.to_regprocedure('guarded.waddle_ingress_refresh_retention(uuid)') IS NOT NULL, \
           pg_catalog.to_regprocedure('guarded.waddle_ingress_refresh_retention(uuid,timestamptz)') IS NULL, \
           pg_catalog.to_regprocedure('guarded.waddle_ingress_settle_descendant(uuid)') IS NOT NULL, \
           pg_catalog.to_regprocedure('guarded.waddle_ingress_settle_descendant(uuid,timestamptz)') IS NULL, \
           pg_catalog.has_function_privilege($1, \
             pg_catalog.to_regprocedure('guarded.waddle_ingress_refresh_retention(uuid)'), 'EXECUTE'), \
           pg_catalog.has_function_privilege($1, \
             pg_catalog.to_regprocedure('guarded.waddle_ingress_settle_descendant(uuid)'), 'EXECUTE')",
    )
    .bind(&fixture.runtime)
    .fetch_one(&mut owner)
    .await
    .expect("inspect timestamp-free retention APIs");
    assert_eq!(timestamp_api_shape, (true, true, true, true, true, true));
    let timestamp_api_uses_database_clock: (bool, bool) = sqlx::query_as(
        "SELECT \
           pg_catalog.strpos(pg_catalog.lower(pg_catalog.pg_get_functiondef( \
             pg_catalog.to_regprocedure('guarded.waddle_ingress_refresh_retention(uuid)'))), \
             'clock_timestamp()') > 0, \
           pg_catalog.strpos(pg_catalog.lower(pg_catalog.pg_get_functiondef( \
             pg_catalog.to_regprocedure('guarded.waddle_ingress_settle_descendant(uuid)'))), \
             'clock_timestamp()') > 0",
    )
    .fetch_one(&mut owner)
    .await
    .expect("inspect database-clock retention implementation");
    assert_eq!(timestamp_api_uses_database_clock, (true, true));
    let function_execute_grantees: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT COALESCE(
            pg_catalog.array_agg(
                COALESCE(r.rolname::text, 'PUBLIC')
                ORDER BY COALESCE(r.rolname::text, 'PUBLIC')
            ),
            ARRAY[]::text[]
        )
        FROM pg_catalog.pg_proc p
        CROSS JOIN LATERAL pg_catalog.aclexplode(
            COALESCE(p.proacl, pg_catalog.acldefault('f', p.proowner))
        ) acl
        LEFT JOIN pg_catalog.pg_roles r ON r.oid = acl.grantee
        WHERE p.oid = pg_catalog.to_regprocedure(
            'guarded.waddle_ingress_resolve_origin(uuid,integer,bytea,bytea,text,integer,text,text,text,uuid,numeric,numeric,uuid,numeric)'
        )
          AND acl.privilege_type = 'EXECUTE'
        "#,
    )
    .fetch_one(&mut owner)
    .await
    .expect("inspect exact function EXECUTE grantees");
    let mut expected_execute_grantees = vec![api_owner.clone(), fixture.runtime.clone()];
    expected_execute_grantees.sort();
    assert_eq!(function_execute_grantees, expected_execute_grantees);
    let callable_api_functions: Vec<(String, String, String, bool, bool, bool, bool)> =
        sqlx::query_as(
            r#"
        SELECT p.proname::text, pg_catalog.pg_get_userbyid(p.proowner)::text,
               p.provolatile::text, p.prosecdef,
               COALESCE('search_path=pg_catalog, pg_temp' = ANY(p.proconfig), false),
               pg_catalog.has_function_privilege($1, p.oid, 'EXECUTE'),
               EXISTS (
                   SELECT 1
                   FROM pg_catalog.aclexplode(
                       COALESCE(p.proacl, pg_catalog.acldefault('f', p.proowner))
                   ) acl
                   WHERE acl.grantee = 0 AND acl.privilege_type = 'EXECUTE'
               )
        FROM pg_catalog.pg_proc p
        JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname = 'guarded'
          AND p.proname LIKE 'waddle_ingress_%'
          AND p.proname NOT IN (
              'waddle_ingress_epoch_guard', 'waddle_ingress_truncate_guard',
              'waddle_ingress_protocol_epoch_guard',
              'waddle_ingress_epoch_guard_manifest_append_only',
              'waddle_ingress_owner_rejection_delete_guard',
              'waddle_ingress_owner_rejection_immutable',
              'waddle_ingress_owner_rejection_insert_guard',
              'waddle_ingress_source_binding_guard',
              'waddle_ingress_source_insert_guard',
              'waddle_ingress_definer_prepare', 'waddle_ingress_definer_epoch_guard'
          )
        ORDER BY p.proname
        "#,
        )
        .bind(&fixture.runtime)
        .fetch_all(&mut owner)
        .await
        .expect("inspect full candidate API function catalog");
    assert!(
        callable_api_functions.len() >= 35,
        "candidate API catalog unexpectedly small: {} functions",
        callable_api_functions.len()
    );
    for (
        name,
        owner_role,
        volatility,
        security_definer,
        safe_path,
        runtime_execute,
        public_execute,
    ) in callable_api_functions
    {
        assert_eq!(owner_role, api_owner, "{name} must have the API owner");
        assert_eq!(
            volatility, "v",
            "{name} must be VOLATILE for epoch preparation"
        );
        assert!(security_definer, "{name} must be SECURITY DEFINER");
        assert!(safe_path, "{name} must pin the safe search_path");
        assert!(runtime_execute, "runtime must execute {name}");
        assert!(!public_execute, "PUBLIC must not execute {name}");
    }
    let can_set_owner: bool = sqlx::query_scalar("SELECT pg_catalog.pg_has_role($1, $2, 'MEMBER')")
        .bind(&fixture.runtime)
        .bind(&api_owner)
        .fetch_one(&mut owner)
        .await
        .expect("inspect runtime owner membership");
    assert!(!can_set_owner);
    let api_has_any_membership: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM pg_catalog.pg_auth_members m
            WHERE m.roleid = (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = $1)
               OR m.member = (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = $1)
        )
        "#,
    )
    .bind(&api_owner)
    .fetch_one(&mut owner)
    .await
    .expect("inspect API owner role memberships");
    assert!(!api_has_any_membership);

    // Leave a malicious same-named type in this backend's pg_temp before the
    // first function call. The fixed function path must resolve built-in TEXT
    // from pg_catalog ahead of PostgreSQL's implicit temporary-type lookup.
    sqlx::query(&format!(
        "GRANT TEMPORARY ON DATABASE {} TO {}",
        fixture.database, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("temporarily grant runtime TEMP for type shadow");
    let mut runtime = fixture.runtime().await;
    sqlx::query("CREATE DOMAIN pg_temp.text AS pg_catalog.text CHECK (VALUE IS NULL)")
        .execute(&mut runtime)
        .await
        .expect("create hostile temporary text domain before API call");
    let has_temp_text_domain: bool =
        sqlx::query_scalar("SELECT pg_catalog.to_regtype('pg_temp.text') IS NOT NULL")
            .fetch_one(&mut runtime)
            .await
            .expect("confirm hostile temporary text domain exists");
    assert!(has_temp_text_domain);
    sqlx::query(&format!(
        "REVOKE TEMPORARY ON DATABASE {} FROM {}",
        fixture.database, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke runtime TEMP while retaining type shadow");

    // A forged epoch proof and real prelock cannot turn the runtime login into
    // a direct writer. Exercise all 13 guarded relations, including TRUNCATE,
    // rather than relying only on catalog ACL inspection.
    for table in TABLES {
        let column = first_column(&mut owner, table).await;
        let statements = [
            format!("INSERT INTO guarded.{table} ({column}) VALUES (NULL)"),
            format!("UPDATE guarded.{table} SET {column} = NULL WHERE false"),
            format!("DELETE FROM guarded.{table} WHERE false"),
            format!("TRUNCATE guarded.{table}"),
        ];
        for statement in statements {
            begin(&mut runtime).await;
            writer_lock(&mut runtime).await;
            forged_proof(&mut runtime).await;
            let error = sqlx::query(&statement)
                .execute(&mut runtime)
                .await
                .expect_err("runtime must lack every guarded direct write privilege");
            assert_eq!(
                error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .as_deref(),
                Some("42501"),
                "{table}: {statement}: {error}"
            );
            sqlx::query("ROLLBACK")
                .execute(&mut runtime)
                .await
                .expect("rollback denied direct write");
        }
    }

    // Ordinary reads may be permitted later, but row-lock reads need the
    // fixed API too. Grant SELECT temporarily so the denial proves the
    // missing UPDATE privilege rather than merely missing read access.
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_sm_streams",
    ] {
        sqlx::query(&format!(
            "GRANT SELECT ON guarded.{table} TO {}",
            fixture.runtime
        ))
        .execute(&mut owner)
        .await
        .expect("grant temporary read-only access for row-lock probe");
    }
    for statement in [
        "SELECT message_key FROM guarded.ingress_messages FOR UPDATE",
        "SELECT message_key FROM guarded.ingress_messages FOR UPDATE SKIP LOCKED",
        "SELECT alias_key_hash FROM guarded.ingress_origin_aliases FOR UPDATE",
        "SELECT sm_ingress_id FROM guarded.ingress_sm_streams FOR SHARE",
    ] {
        let error = sqlx::query(statement)
            .execute(&mut runtime)
            .await
            .expect_err("direct runtime row-lock read must be denied");
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("42501"),
            "{statement}: {error}"
        );
    }
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_sm_streams",
    ] {
        sqlx::query(&format!(
            "REVOKE SELECT ON guarded.{table} FROM {}",
            fixture.runtime
        ))
        .execute(&mut owner)
        .await
        .expect("restore restricted runtime read profile");
    }

    // A function call without the prepared epoch lock fails before it can
    // create either canonical row.
    let no_lock_origin = format!("no-lock-{suffix}");
    let no_lock_hash = alias_hash(&sender, "target@example.test", 1, &no_lock_origin);
    let no_lock_key = uuid::Uuid::new_v4();
    begin(&mut runtime).await;
    let no_lock_error = sqlx::query(
        "SELECT * FROM guarded.waddle_ingress_resolve_origin(\
           $1, 1, $2, $3, $4, 1, 'target@example.test', $5, \
           $4, $6, 1, 1, NULL, NULL)",
    )
    .bind(no_lock_key)
    .bind(vec![7_u8; 32])
    .bind(no_lock_hash.to_vec())
    .bind(&sender)
    .bind(&no_lock_origin)
    .bind(context_a)
    .execute(&mut runtime)
    .await
    .expect_err("function must require epoch-first prelock");
    assert!(no_lock_error
        .to_string()
        .contains("epoch ACCESS SHARE prelock"));
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback missing-prelock call");
    let no_lock_rows: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM guarded.ingress_messages WHERE message_key = $1) + \
                (SELECT count(*) FROM guarded.ingress_origin_aliases WHERE alias_key_hash = $2)",
    )
    .bind(no_lock_key)
    .bind(no_lock_hash.to_vec())
    .fetch_one(&mut owner)
    .await
    .expect("check missing-prelock call has no row effects");
    assert_eq!(no_lock_rows, 0);

    // Read API bounds are fail-closed. A NULL LIMIT is unbounded in PostgreSQL,
    // so reject it alongside zero and values beyond the bounded page contract.
    for page_size in [None, Some(0), Some(65)] {
        begin(&mut runtime).await;
        writer_lock(&mut runtime).await;
        proof(&mut runtime, 0).await;
        let error = sqlx::query(
            "SELECT * FROM guarded.waddle_ingress_list_legacy_ancestry_intents(\
               NULL::uuid, NULL::integer, NULL::bytea, $1::integer)",
        )
        .bind(page_size)
        .fetch_all(&mut runtime)
        .await
        .expect_err("malformed or oversized ancestry pages must be rejected");
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("22023"),
            "page size {page_size:?}: {error}"
        );
        sqlx::query("ROLLBACK")
            .execute(&mut runtime)
            .await
            .expect("rollback invalid ancestry page request");
    }
    let bounded_page_calls = [
        format!(
            "SELECT * FROM guarded.waddle_ingress_list_effect_intents('{legacy_key}'::uuid, NULL::numeric, $1::integer)"
        ),
        format!(
            "SELECT * FROM guarded.waddle_ingress_list_effect_receipts('{legacy_key}'::uuid, NULL::integer, NULL::bytea, $1::integer)"
        ),
        format!(
            "SELECT * FROM guarded.waddle_ingress_list_carbon_receipts('{legacy_key}'::uuid, 7, decode(repeat('00', 32), 'hex'), NULL::text, $1::integer)"
        ),
        format!(
            "SELECT * FROM guarded.waddle_ingress_list_pending_send_attempts('{legacy_key}'::uuid, 7, decode(repeat('00', 32), 'hex'), NULL::text, $1::integer)"
        ),
        format!(
            "SELECT * FROM guarded.waddle_ingress_list_delivery_receipts('{legacy_key}'::uuid, 7, decode(repeat('00', 32), 'hex'), NULL::text, $1::integer)"
        ),
        format!(
            "SELECT * FROM guarded.waddle_ingress_list_all_delivery_receipts('{legacy_key}'::uuid, 7, decode(repeat('00', 32), 'hex'), NULL::text, $1::integer)"
        ),
        format!(
            "SELECT * FROM guarded.waddle_ingress_list_archive_positions('{legacy_key}'::uuid, 1, decode(repeat('00', 32), 'hex'), NULL::text, NULL::bigint, $1::integer)"
        ),
    ];
    for sql in bounded_page_calls {
        for page_size in [None, Some(0), Some(257)] {
            begin(&mut runtime).await;
            writer_lock(&mut runtime).await;
            proof(&mut runtime, 0).await;
            let error = sqlx::query(&sql)
                .bind(page_size)
                .fetch_all(&mut runtime)
                .await
                .expect_err("malformed or oversized API page must be rejected");
            assert_eq!(
                error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .as_deref(),
                Some("22023"),
                "{sql}, page size {page_size:?}: {error}"
            );
            sqlx::query("ROLLBACK")
                .execute(&mut runtime)
                .await
                .expect("rollback invalid API page request");
        }
    }
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let inserted_intents: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generate_series(1, 257) AS series(ordinal) \
         WHERE guarded.waddle_ingress_insert_effect_intent( \
             $1, ordinal::numeric, ordinal::integer, \
             decode(lpad(to_hex(ordinal), 64, '0'), 'hex'), decode('00', 'hex'))",
    )
    .bind(legacy_key)
    .fetch_one(&mut runtime)
    .await
    .expect("seed more effect intents than one bounded page");
    assert_eq!(inserted_intents, 257);
    let first_page: Vec<String> = sqlx::query_scalar(
        "SELECT effect_ordinal FROM guarded.waddle_ingress_list_effect_intents( \
             $1, NULL::numeric, 256)",
    )
    .bind(legacy_key)
    .fetch_all(&mut runtime)
    .await
    .expect("read first bounded intent page");
    assert_eq!(first_page.len(), 256);
    assert_eq!(first_page.first().map(String::as_str), Some("1"));
    assert_eq!(first_page.last().map(String::as_str), Some("256"));
    let second_page: Vec<String> = sqlx::query_scalar(
        "SELECT effect_ordinal FROM guarded.waddle_ingress_list_effect_intents( \
             $1, 256::numeric, 256)",
    )
    .bind(legacy_key)
    .fetch_all(&mut runtime)
    .await
    .expect("read second bounded intent page");
    assert_eq!(second_page, vec!["257"]);
    let exhausted: Vec<String> = sqlx::query_scalar(
        "SELECT effect_ordinal FROM guarded.waddle_ingress_list_effect_intents( \
             $1, 257::numeric, 256)",
    )
    .bind(legacy_key)
    .fetch_all(&mut runtime)
    .await
    .expect("read exhausted bounded intent page");
    assert!(exhausted.is_empty());
    let mut archive_hash = vec![0_u8; 32];
    archive_hash[31] = 1;
    let inserted_archive: bool = sqlx::query_scalar(
        "SELECT guarded.waddle_ingress_insert_archive_dispatch( \
             'archive@example.test', 1, $1, 1, $2, 'resource@example.test', '')",
    )
    .bind(legacy_key)
    .bind(&archive_hash)
    .fetch_one(&mut runtime)
    .await
    .expect("insert archive dispatch through fixed API");
    assert!(inserted_archive);
    let stored_archive_position: Option<i64> = sqlx::query_scalar(
        "SELECT guarded.waddle_ingress_archive_dispatch_position( \
             'archive@example.test', $1, 1, $2, 'resource@example.test', '')",
    )
    .bind(legacy_key)
    .bind(&archive_hash)
    .fetch_one(&mut runtime)
    .await
    .expect("read archive dispatch through fixed API");
    assert_eq!(stored_archive_position, Some(1));
    let archive_positions: Vec<(String, i64)> = sqlx::query_as(
        "SELECT archive_jid, archive_seq FROM guarded.waddle_ingress_list_archive_positions( \
             $1, 1, $2, NULL::text, NULL::bigint, 256)",
    )
    .bind(legacy_key)
    .bind(&archive_hash)
    .fetch_all(&mut runtime)
    .await
    .expect("read bounded archive position page");
    assert_eq!(archive_positions, vec![("archive@example.test".into(), 1)]);
    let terminal: bool =
        sqlx::query_scalar("SELECT guarded.waddle_ingress_message_is_terminal($1)")
            .bind(legacy_key)
            .fetch_one(&mut runtime)
            .await
            .expect("read terminal state through fixed API");
    assert!(!terminal);
    let created_at: String =
        sqlx::query_scalar("SELECT created_at FROM guarded.waddle_ingress_message_created_at($1)")
            .bind(legacy_key)
            .fetch_one(&mut runtime)
            .await
            .expect("read message creation timestamp through fixed API");
    assert!(created_at.ends_with('Z'));
    let initial_envelope: (Option<i32>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT envelope_version, envelope FROM guarded.waddle_ingress_load_message_envelope($1)",
    )
    .bind(legacy_key)
    .fetch_one(&mut runtime)
    .await
    .expect("read absent message envelope through fixed API");
    assert_eq!(initial_envelope, (None, None));
    let envelope = b"<message/>".to_vec();
    let envelope_updated: bool = sqlx::query_scalar(
        "SELECT guarded.waddle_ingress_update_message_envelope($1, 1::smallint, $2)",
    )
    .bind(legacy_key)
    .bind(&envelope)
    .fetch_one(&mut runtime)
    .await
    .expect("update message envelope through fixed API");
    assert!(envelope_updated);
    let stored_envelope: (Option<i32>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT envelope_version, envelope FROM guarded.waddle_ingress_load_message_envelope($1)",
    )
    .bind(legacy_key)
    .fetch_one(&mut runtime)
    .await
    .expect("read updated message envelope through fixed API");
    assert_eq!(stored_envelope, (Some(1), Some(envelope)));
    let source_key = uuid::Uuid::new_v4();
    let child_key = uuid::Uuid::new_v4();
    let bound_source_inserted: bool = sqlx::query_scalar(
        "SELECT guarded.waddle_ingress_insert_message( \
             $1, 1, decode(repeat('11', 32), 'hex'), NULL::smallint, NULL::bytea, \
             $2, $3, 1::numeric, 1::numeric, NULL::uuid, NULL::numeric)",
    )
    .bind(source_key)
    .bind(&sender)
    .bind(context_a)
    .fetch_one(&mut runtime)
    .await
    .expect("insert bound source through fixed API");
    assert!(bound_source_inserted);
    let child_inserted: bool = sqlx::query_scalar(
        "SELECT guarded.waddle_ingress_insert_message( \
             $1, 1, decode(repeat('22', 32), 'hex'), NULL::smallint, NULL::bytea, \
             NULL::text, NULL::uuid, NULL::numeric, NULL::numeric, NULL::uuid, NULL::numeric)",
    )
    .bind(child_key)
    .fetch_one(&mut runtime)
    .await
    .expect("insert child through fixed API");
    assert!(child_inserted);
    let initialized: bool =
        sqlx::query_scalar("SELECT guarded.waddle_ingress_initialize_owner_decision($1)")
            .bind(source_key)
            .fetch_one(&mut runtime)
            .await
            .expect("initialize owner decision through fixed API");
    assert!(initialized);
    let rejected: bool =
        sqlx::query_scalar("SELECT guarded.waddle_ingress_record_owner_rejection($1, $2)")
            .bind(source_key)
            .bind(child_key)
            .fetch_one(&mut runtime)
            .await
            .expect("record owner rejection through fixed API");
    assert!(rejected);
    let stored_child: Option<String> =
        sqlx::query_scalar("SELECT guarded.waddle_ingress_owner_rejection_child($1)")
            .bind(source_key)
            .fetch_one(&mut runtime)
            .await
            .expect("read owner rejection through fixed API");
    let expected_child = child_key.to_string();
    assert_eq!(stored_child.as_deref(), Some(expected_child.as_str()));
    let settled: bool = sqlx::query_scalar("SELECT guarded.waddle_ingress_settle_descendant($1)")
        .bind(retention_descendant)
        .fetch_one(&mut runtime)
        .await
        .expect("settle descendant with database-controlled time");
    assert!(settled);
    let refreshed: bool = sqlx::query_scalar("SELECT guarded.waddle_ingress_refresh_retention($1)")
        .bind(retention_key)
        .fetch_one(&mut runtime)
        .await
        .expect("refresh retention with database-controlled time");
    assert!(refreshed);
    commit(&mut runtime).await;
    let retention_clock_is_current: (bool, bool, bool) = sqlx::query_as(
        "SELECT terminal_at < pg_catalog.clock_timestamp() - interval '8 days', \
           settled_at > pg_catalog.clock_timestamp() - interval '5 seconds', \
           retention_eligible_at > pg_catalog.clock_timestamp() - interval '5 seconds' \
         FROM guarded.ingress_messages AS m \
         JOIN guarded.ingress_effect_descendants AS d ON d.message_key = m.message_key \
         WHERE m.message_key = $1 AND d.descendant_key = $2",
    )
    .bind(retention_key)
    .bind(retention_descendant.to_string())
    .fetch_one(&mut owner)
    .await
    .expect("verify retention starts at database time, not the old terminal time");
    assert_eq!(retention_clock_is_current, (true, true, true));
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let null_filter = sqlx::query(
        "SELECT guarded.waddle_ingress_descendant_exists('legacy-value', NULL::boolean)",
    )
    .fetch_one(&mut runtime)
    .await
    .expect_err("nullable descendant filter must be rejected");
    assert_eq!(
        null_filter
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("22023"),
        "{null_filter}"
    );
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback null descendant filter");

    // A caller-selected source tuple is not accepted merely because its
    // fields are well-formed: its durable session must still be live.
    let expired_origin = format!("expired-principal-{suffix}");
    let expired_hash = alias_hash(&sender, "target@example.test", 1, &expired_origin);
    let expired_key = uuid::Uuid::new_v4();
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let (expired_returned, expired_outcome, _, _) = resolve_origin(
        &mut runtime,
        OriginRequest {
            message_key: expired_key,
            hash: expired_hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &expired_origin,
            digest: [33_u8; 32],
            source_context: Some(expired_context),
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(expired_returned, None);
    assert_eq!(expired_outcome, "principal_assertion_failed");
    commit(&mut runtime).await;
    let expired_rows: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM guarded.ingress_messages WHERE message_key = $1) + \
                (SELECT count(*) FROM guarded.ingress_origin_aliases WHERE alias_key_hash = $2)",
    )
    .bind(expired_key)
    .bind(expired_hash.to_vec())
    .fetch_one(&mut owner)
    .await
    .expect("check expired-principal call has no row effects");
    assert_eq!(expired_rows, 0);

    // Prepare TEMP shadows under a temporary grant, then revoke TEMP while
    // retaining them in this session. Function and trigger SQL must still hit
    // the schema-qualified real relations with a hostile search_path.
    sqlx::query(&format!(
        "GRANT TEMPORARY ON DATABASE {} TO {}",
        fixture.database, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("temporarily grant runtime TEMP");
    sqlx::query("CREATE TEMP TABLE ingress_protocol_epoch (id integer, epoch bigint)")
        .execute(&mut runtime)
        .await
        .expect("create hostile epoch shadow");
    sqlx::query("INSERT INTO pg_temp.ingress_protocol_epoch VALUES (1, 999)")
        .execute(&mut runtime)
        .await
        .expect("seed hostile epoch shadow");
    sqlx::query(
        "CREATE TEMP TABLE ingress_messages (message_key uuid, digest_version int, digest bytea)",
    )
    .execute(&mut runtime)
    .await
    .expect("create hostile message shadow");
    sqlx::query(
        "CREATE TEMP TABLE ingress_origin_aliases (alias_key_hash bytea, message_key uuid)",
    )
    .execute(&mut runtime)
    .await
    .expect("create hostile alias shadow");
    sqlx::query(&format!(
        "REVOKE TEMPORARY ON DATABASE {} FROM {}",
        fixture.database, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke TEMP with old session shadows intact");

    let origin = format!("valid-{suffix}");
    let hash = alias_hash(&sender, "target@example.test", 1, &origin);
    let digest = [31_u8; 32];
    let key = uuid::Uuid::new_v4();
    begin(&mut runtime).await;
    sqlx::query("SET LOCAL search_path = pg_temp, guarded")
        .execute(&mut runtime)
        .await
        .expect("prefer TEMP shadows");
    writer_lock(&mut runtime).await;
    forged_proof(&mut runtime).await;
    let (inserted_key, outcome, digest_version, stored_digest) = resolve_origin(
        &mut runtime,
        OriginRequest {
            message_key: key,
            hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &origin,
            digest,
            source_context: Some(context_a),
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(inserted_key, Some(key));
    assert_eq!(outcome, "inserted");
    assert_eq!(digest_version, Some(1));
    assert_eq!(stored_digest, Some(digest.to_vec()));
    let mut observer = fixture.observer().await;
    let visible_before_commit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM guarded.ingress_messages WHERE message_key = $1")
            .bind(key)
            .fetch_one(&mut observer)
            .await
            .expect("check outer transaction visibility");
    assert_eq!(
        visible_before_commit, 0,
        "function must not commit internally"
    );
    commit(&mut runtime).await;
    let visible_after_commit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM guarded.ingress_messages WHERE message_key = $1")
            .bind(key)
            .fetch_one(&mut observer)
            .await
            .expect("check caller commit visibility");
    assert_eq!(visible_after_commit, 1);

    // The same alias and digest replay to the original key. A changed digest,
    // source principal or raw identity is classified without a partial write.
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let (replay_key, replay_outcome, _, _) = resolve_origin(
        &mut runtime,
        OriginRequest {
            message_key: uuid::Uuid::new_v4(),
            hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &origin,
            digest,
            source_context: Some(context_a),
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(replay_key, Some(key));
    assert_eq!(replay_outcome, "existing");
    commit(&mut runtime).await;

    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let (_, digest_outcome, conflict_version, conflict_digest) = resolve_origin(
        &mut runtime,
        OriginRequest {
            message_key: uuid::Uuid::new_v4(),
            hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &origin,
            digest: [32_u8; 32],
            source_context: Some(context_a),
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(digest_outcome, "digest_conflict");
    assert_eq!(conflict_version, Some(1));
    assert_eq!(conflict_digest, Some(digest.to_vec()));
    commit(&mut runtime).await;

    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let (_, source_outcome, _, _) = resolve_origin(
        &mut runtime,
        OriginRequest {
            message_key: uuid::Uuid::new_v4(),
            hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &origin,
            digest,
            source_context: Some(context_b),
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(source_outcome, "source_binding_conflict");
    commit(&mut runtime).await;

    // Omitting source data cannot turn a previously bound alias into an
    // unbound replay. The legacy unbound fixture below remains replayable.
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let (_, omitted_bound_source_outcome, _, _) = resolve_origin(
        &mut runtime,
        OriginRequest {
            message_key: uuid::Uuid::new_v4(),
            hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &origin,
            digest,
            source_context: None,
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(omitted_bound_source_outcome, "source_binding_conflict");
    commit(&mut runtime).await;

    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let (legacy_replay_key, legacy_replay_outcome, _, _) = resolve_origin(
        &mut runtime,
        OriginRequest {
            message_key: uuid::Uuid::new_v4(),
            hash: legacy_hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &legacy_origin,
            digest: legacy_digest,
            source_context: None,
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(legacy_replay_key, Some(legacy_key));
    assert_eq!(legacy_replay_outcome, "existing");
    commit(&mut runtime).await;

    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let sm_without_principal = sqlx::query(
        "SELECT * FROM guarded.waddle_ingress_resolve_origin(\
           $1, 1, $2, $3, $4, 1, 'target@example.test', $5, \
           NULL, NULL, NULL, NULL, $6, 1)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(digest.to_vec())
    .bind(hash.to_vec())
    .bind(&sender)
    .bind(&origin)
    .bind(uuid::Uuid::new_v4());
    let sm_error = sm_without_principal
        .fetch_one(&mut runtime)
        .await
        .expect_err("SM occurrence without principal must be rejected");
    assert!(sm_error
        .to_string()
        .contains("incomplete canonical source binding"));
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback malformed source call");

    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let (_, identity_outcome, _, _) = resolve_origin(
        &mut runtime,
        OriginRequest {
            message_key: uuid::Uuid::new_v4(),
            hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: "different-origin",
            digest,
            source_context: Some(context_a),
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(identity_outcome, "alias_identity_conflict");
    commit(&mut runtime).await;

    // An explicit caller rollback undoes both rows created by the API.
    let rolled_origin = format!("rolled-back-{suffix}");
    let rolled_hash = alias_hash(&sender, "target@example.test", 1, &rolled_origin);
    let rolled_key = uuid::Uuid::new_v4();
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let (rolled_returned, rolled_outcome, _, _) = resolve_origin(
        &mut runtime,
        OriginRequest {
            message_key: rolled_key,
            hash: rolled_hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &rolled_origin,
            digest,
            source_context: Some(context_a),
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(rolled_returned, Some(rolled_key));
    assert_eq!(rolled_outcome, "inserted");
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("caller rollback");
    let rolled_rows: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM guarded.ingress_messages WHERE message_key = $1) + \
                (SELECT count(*) FROM guarded.ingress_origin_aliases WHERE alias_key_hash = $2)",
    )
    .bind(rolled_key)
    .bind(rolled_hash.to_vec())
    .fetch_one(&mut observer)
    .await
    .expect("check rolled-back rows");
    assert_eq!(rolled_rows, 0);

    // Two simultaneous first writers converge on one alias; the losing
    // candidate is rolled back inside the fixed API's exception block.
    let race_origin = format!("race-{suffix}");
    let race_hash = alias_hash(&sender, "target@example.test", 1, &race_origin);
    let winner_key = uuid::Uuid::new_v4();
    let loser_key = uuid::Uuid::new_v4();
    let race_sender = sender.clone();
    let race_origin_task = race_origin.clone();
    let mut winner = fixture.runtime().await;
    begin(&mut winner).await;
    writer_lock(&mut winner).await;
    proof(&mut winner, 0).await;
    let (winner_result, winner_outcome, _, _) = resolve_origin(
        &mut winner,
        OriginRequest {
            message_key: winner_key,
            hash: race_hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &race_origin,
            digest,
            source_context: Some(context_a),
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(winner_result, Some(winner_key));
    assert_eq!(winner_outcome, "inserted");

    let mut loser = fixture.runtime().await;
    begin(&mut loser).await;
    writer_lock(&mut loser).await;
    proof(&mut loser, 0).await;
    let loser_pid = pid(&mut loser).await;
    let winner_pid = pid(&mut winner).await;
    let mut wait_observer = fixture.observer().await;
    let loser_task = tokio::spawn(async move {
        let result = resolve_origin(
            &mut loser,
            OriginRequest {
                message_key: loser_key,
                hash: race_hash,
                sender: &race_sender,
                target_jid: "target@example.test",
                origin_id: &race_origin_task,
                digest,
                source_context: Some(context_a),
                source_sm_ingress_id: None,
            },
        )
        .await;
        commit(&mut loser).await;
        (loser, result)
    });
    wait_blocked(&mut wait_observer, loser_pid, winner_pid).await;
    commit(&mut winner).await;
    let (mut loser, (loser_result, loser_outcome, _, _)) =
        loser_task.await.expect("join concurrent alias loser");
    assert_eq!(loser_result, Some(winner_key));
    assert_eq!(loser_outcome, "existing");
    let race_counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM guarded.ingress_messages WHERE message_key IN ($1, $2)), \
                (SELECT count(*) FROM guarded.ingress_origin_aliases WHERE alias_key_hash = $3)",
    )
    .bind(winner_key)
    .bind(loser_key)
    .bind(race_hash.to_vec())
    .fetch_one(&mut observer)
    .await
    .expect("check concurrency winner rows");
    assert_eq!(race_counts, (1, 1));

    // If the blocked first writer rolls back, the waiting writer becomes the
    // canonical winner without leaving the abandoned candidate behind.
    let aborted_origin = format!("aborted-winner-{suffix}");
    let aborted_hash = alias_hash(&sender, "target@example.test", 1, &aborted_origin);
    let aborted_key = uuid::Uuid::new_v4();
    let retry_key = uuid::Uuid::new_v4();
    let aborted_sender = sender.clone();
    let aborted_origin_task = aborted_origin.clone();
    begin(&mut winner).await;
    writer_lock(&mut winner).await;
    proof(&mut winner, 0).await;
    let (aborted_result, aborted_outcome, _, _) = resolve_origin(
        &mut winner,
        OriginRequest {
            message_key: aborted_key,
            hash: aborted_hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &aborted_origin,
            digest,
            source_context: Some(context_a),
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(aborted_result, Some(aborted_key));
    assert_eq!(aborted_outcome, "inserted");
    begin(&mut loser).await;
    writer_lock(&mut loser).await;
    proof(&mut loser, 0).await;
    let loser_pid = pid(&mut loser).await;
    let winner_pid = pid(&mut winner).await;
    let retry_task = tokio::spawn(async move {
        let result = resolve_origin(
            &mut loser,
            OriginRequest {
                message_key: retry_key,
                hash: aborted_hash,
                sender: &aborted_sender,
                target_jid: "target@example.test",
                origin_id: &aborted_origin_task,
                digest,
                source_context: Some(context_a),
                source_sm_ingress_id: None,
            },
        )
        .await;
        commit(&mut loser).await;
        (loser, result)
    });
    wait_blocked(&mut wait_observer, loser_pid, winner_pid).await;
    sqlx::query("ROLLBACK")
        .execute(&mut winner)
        .await
        .expect("abort first alias writer");
    let (loser, (retry_result, retry_outcome, _, _)) =
        retry_task.await.expect("join waiting alias writer");
    assert_eq!(retry_result, Some(retry_key));
    assert_eq!(retry_outcome, "inserted");
    let aborted_counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM guarded.ingress_messages WHERE message_key IN ($1, $2)), \
                (SELECT count(*) FROM guarded.ingress_origin_aliases WHERE alias_key_hash = $3)",
    )
    .bind(aborted_key)
    .bind(retry_key)
    .bind(aborted_hash.to_vec())
    .fetch_one(&mut observer)
    .await
    .expect("check aborted winner left no row");
    assert_eq!(aborted_counts, (1, 1));

    // The alias-race handler must not consume a different unique violation
    // raised while inserting the tentative message.
    sqlx::query(
        "CREATE UNIQUE INDEX fixture_unique_digest ON guarded.ingress_messages (digest) \
         WHERE digest = decode(repeat('7f', 32), 'hex')",
    )
    .execute(&mut owner)
    .await
    .expect("install unrelated unique index in disposable schema");
    let unique_digest = vec![0x7f_u8; 32];
    let first_unique_key = uuid::Uuid::new_v4();
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let first_unique_insert: bool = sqlx::query_scalar(
        "SELECT guarded.waddle_ingress_insert_message( \
             $1, 1, $2, NULL::smallint, NULL::bytea, \
             NULL::text, NULL::uuid, NULL::numeric, NULL::numeric, NULL::uuid, NULL::numeric)",
    )
    .bind(first_unique_key)
    .bind(&unique_digest)
    .fetch_one(&mut runtime)
    .await
    .expect("seed one unique digest through fixed API");
    assert!(first_unique_insert);
    commit(&mut runtime).await;
    let duplicate_unique_key = uuid::Uuid::new_v4();
    let duplicate_unique_origin = format!("different-unique-{suffix}");
    let duplicate_unique_hash =
        alias_hash(&sender, "target@example.test", 1, &duplicate_unique_origin);
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    proof(&mut runtime, 0).await;
    let unrelated_unique = sqlx::query(
        "SELECT canonical_message_key FROM guarded.waddle_ingress_resolve_origin( \
             $1, 1, $2, $3, $4, 1, 'target@example.test', $5, \
             NULL::text, NULL::uuid, NULL::numeric, NULL::numeric, NULL::uuid, NULL::numeric)",
    )
    .bind(duplicate_unique_key)
    .bind(&unique_digest)
    .bind(duplicate_unique_hash.to_vec())
    .bind(&sender)
    .bind(&duplicate_unique_origin)
    .fetch_one(&mut runtime)
    .await
    .expect_err("unrelated unique violation must propagate");
    let unrelated_database_error = unrelated_unique
        .as_database_error()
        .expect("PostgreSQL reports unrelated unique violation");
    assert_eq!(unrelated_database_error.code().as_deref(), Some("23505"));
    assert_eq!(
        unrelated_database_error.constraint(),
        Some("fixture_unique_digest")
    );
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback unrelated unique violation");
    let duplicate_unique_rows: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM guarded.ingress_messages WHERE message_key = $1) + \
                (SELECT count(*) FROM guarded.ingress_origin_aliases WHERE alias_key_hash = $2)",
    )
    .bind(duplicate_unique_key)
    .bind(duplicate_unique_hash.to_vec())
    .fetch_one(&mut observer)
    .await
    .expect("check unrelated violation left no rows");
    assert_eq!(duplicate_unique_rows, 0);

    // Ordinary runtime lacks DML, even after a forged proof and an actual
    // epoch lock. With temporary DML, the invoker trigger independently
    // rejects the same runtime identity inside the guard.
    begin(&mut runtime).await;
    writer_lock(&mut runtime).await;
    forged_proof(&mut runtime).await;
    let acl_denial = sqlx::query(
        "INSERT INTO guarded.ingress_messages (message_key, digest_version, digest) \
         VALUES ($1, 1, $2)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(digest.to_vec())
    .execute(&mut runtime)
    .await
    .expect_err("runtime has no direct DML grant");
    assert_eq!(
        acl_denial
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42501")
    );
    sqlx::query("ROLLBACK")
        .execute(&mut runtime)
        .await
        .expect("rollback direct DML ACL denial");

    for table in ["ingress_messages", "ingress_origin_aliases"] {
        sqlx::query(&format!(
            "GRANT INSERT, UPDATE, DELETE ON guarded.{table} TO {}",
            fixture.runtime
        ))
        .execute(&mut owner)
        .await
        .expect("temporarily grant direct DML for invoker trigger probe");
    }
    let direct_statements = [
        (
            "message-insert",
            "INSERT INTO guarded.ingress_messages (message_key, digest_version, digest) VALUES ($1, 1, $2)",
        ),
        (
            "message-update",
            "UPDATE guarded.ingress_messages SET digest = ''::bytea WHERE false",
        ),
        ("message-delete", "DELETE FROM guarded.ingress_messages WHERE false"),
        (
            "alias-insert",
            "INSERT INTO guarded.ingress_origin_aliases (alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key) VALUES ($1, 'x@example.test', 1, 'target@example.test', 'x', $2)",
        ),
        (
            "alias-update",
            "UPDATE guarded.ingress_origin_aliases SET origin_id = 'replacement' WHERE false",
        ),
        ("alias-delete", "DELETE FROM guarded.ingress_origin_aliases WHERE false"),
    ];
    for (case, statement) in direct_statements {
        begin(&mut runtime).await;
        writer_lock(&mut runtime).await;
        forged_proof(&mut runtime).await;
        let result = match case {
            "message-insert" => {
                sqlx::query(statement)
                    .bind(uuid::Uuid::new_v4())
                    .bind(digest.to_vec())
                    .execute(&mut runtime)
                    .await
            }
            "alias-insert" => {
                sqlx::query(statement)
                    .bind(vec![11_u8; 32])
                    .bind(uuid::Uuid::new_v4())
                    .execute(&mut runtime)
                    .await
            }
            _ => sqlx::query(statement).execute(&mut runtime).await,
        };
        let error = result.expect_err("direct runtime statement must hit invoker identity guard");
        assert_eq!(
            error
                .as_database_error()
                .and_then(|database_error| database_error.code())
                .as_deref(),
            Some("42501"),
            "{case}: {error}"
        );
        assert!(
            error
                .to_string()
                .contains("canonical ingress writes require the API owner"),
            "{case}: {error}"
        );
        sqlx::query("ROLLBACK")
            .execute(&mut runtime)
            .await
            .expect("rollback denied direct DML");
    }
    for table in ["ingress_messages", "ingress_origin_aliases"] {
        sqlx::query(&format!(
            "REVOKE INSERT, UPDATE, DELETE ON guarded.{table} FROM {}",
            fixture.runtime
        ))
        .execute(&mut owner)
        .await
        .expect("restore runtime DML denial");
    }
    assert_no_guarded_dml(&mut owner, &fixture.runtime).await;

    // An epoch-first API writer drains before the transition's AX lock and
    // both transactions complete without upgrading from a target-table lock.
    let barrier_origin = format!("barrier-{suffix}");
    let barrier_hash = alias_hash(&sender, "target@example.test", 1, &barrier_origin);
    let mut barrier_writer = fixture.runtime().await;
    begin(&mut barrier_writer).await;
    writer_lock(&mut barrier_writer).await;
    proof(&mut barrier_writer, 0).await;
    let barrier_key = uuid::Uuid::new_v4();
    let (barrier_result, barrier_outcome, _, _) = resolve_origin(
        &mut barrier_writer,
        OriginRequest {
            message_key: barrier_key,
            hash: barrier_hash,
            sender: &sender,
            target_jid: "target@example.test",
            origin_id: &barrier_origin,
            digest,
            source_context: Some(context_a),
            source_sm_ingress_id: None,
        },
    )
    .await;
    assert_eq!(barrier_result, Some(barrier_key));
    assert_eq!(barrier_outcome, "inserted");
    let writer_pid = pid(&mut barrier_writer).await;
    let mut transition = fixture.owner().await;
    let transition_pid = pid(&mut transition).await;
    let transition_task = tokio::spawn(async move {
        begin(&mut transition).await;
        owner_lock(&mut transition).await;
        advance(&mut transition).await;
        commit(&mut transition).await;
    });
    let mut barrier_observer = fixture.observer().await;
    wait_blocked(&mut barrier_observer, transition_pid, writer_pid).await;
    commit(&mut barrier_writer).await;
    transition_task.await.expect("join AX epoch transition");
    let transitioned_epoch: i64 =
        sqlx::query_scalar("SELECT epoch FROM guarded.ingress_protocol_epoch WHERE id = 1")
            .fetch_one(&mut observer)
            .await
            .expect("read transitioned epoch");
    assert_eq!(transitioned_epoch, 1);

    drop((runtime, owner, observer, wait_observer, loser));
    fixture.cleanup().await;
}
