//! Real PostgreSQL checks for the dark P2.1 physical-connection gate.

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{
    self, AttestedLineage, DeploymentUuid, LineageUuid, PgDatabaseName, PgSchemaName,
};
use waddle_server::db::physical_attestation::{
    attest_connection, attested_pool, install, PgRoleName, PhysicalAttestationConfig,
    PhysicalAttestationError,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};

// Every installer edits pg_control_system's catalog ACL. Cargo test runs
// cases in this binary concurrently; keep those DDL transactions distinct.
static CATALOG_GRANT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    admin: PgPool,
    admin_options: PgConnectOptions,
    runtime_options: PgConnectOptions,
    base_url: String,
    lineage: AttestedLineage,
    target: String,
    trusted: String,
    definer: String,
    runtime: String,
    outsider: String,
}

impl Fixture {
    async fn new(url: String) -> Self {
        let admin = PgPool::connect(&url).await.expect("admin PostgreSQL pool");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let target = format!("waddle_attest_t_{suffix}");
        let trusted = format!("waddle_attest_f_{suffix}");
        let definer = format!("waddle_attest_d_{suffix}");
        let runtime = format!("waddle_attest_r_{suffix}");
        let outsider = format!("waddle_attest_o_{suffix}");
        sqlx::query(&format!("CREATE SCHEMA {target}"))
            .execute(&admin)
            .await
            .expect("target schema");
        for role in [&runtime, &outsider] {
            sqlx::query(&format!("CREATE ROLE {role} LOGIN"))
                .execute(&admin)
                .await
                .expect("test login role");
        }
        sqlx::query(&format!("GRANT USAGE ON SCHEMA {target} TO {runtime}"))
            .execute(&admin)
            .await
            .expect("runtime schema usage");

        let boundary_url = url_with_schema(&url, &target);
        let database = Database::from_config(
            "physical attestation fixture",
            &DatabaseConfig::new(DatabaseDriver::Postgres, boundary_url.clone()),
        )
        .await
        .expect("boundary database");
        let lineage = lineage::enroll(
            &database,
            &LineageConfig {
                deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
                action: None,
            },
        )
        .await
        .expect("enroll real lineage");
        drop(database);

        let admin_options = PgConnectOptions::from_str(&boundary_url).expect("admin options");
        let runtime_options = admin_options.clone().username(&runtime);
        Self {
            admin,
            admin_options,
            runtime_options,
            base_url: url,
            lineage,
            target,
            trusted,
            definer,
            runtime,
            outsider,
        }
    }

    fn config(&self) -> PhysicalAttestationConfig {
        self.with_lineage(self.lineage.clone())
    }

    fn with_lineage(&self, lineage: AttestedLineage) -> PhysicalAttestationConfig {
        PhysicalAttestationConfig::new(
            lineage,
            PgRoleName::new(self.runtime.clone()).expect("test runtime role"),
            PgRoleName::new(self.definer.clone()).expect("test definer role"),
            PgSchemaName(self.trusted.clone()),
        )
        .expect("valid attestation configuration")
    }

    async fn cleanup(self) {
        let definer_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = $1)",
        )
        .bind(&self.definer)
        .fetch_one(&self.admin)
        .await
        .expect("check definer role");
        if definer_exists {
            sqlx::query(&format!(
                "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {}",
                self.definer
            ))
            .execute(&self.admin)
            .await
            .expect("revoke definer catalog grant");
        }
        for schema in [&self.trusted, &self.target] {
            sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
                .execute(&self.admin)
                .await
                .expect("drop test schema");
        }
        for role in [&self.runtime, &self.outsider, &self.definer] {
            sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
                .execute(&self.admin)
                .await
                .expect("drop test role");
        }
        self.admin.close().await;
    }
}

#[tokio::test]
async fn identity_variants_missing_function_and_login_mismatch_fail_closed() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let config = install(&fixture.admin_options, fixture.config())
        .await
        .expect("install attestation");
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    attest_connection(&mut runtime, &config)
        .await
        .expect("matching runtime identity");

    let mut wrong = fixture.lineage.clone();
    wrong.deployment_uuid = DeploymentUuid(uuid::Uuid::new_v4());
    assert!(
        attest_connection(&mut runtime, &fixture.with_lineage(wrong))
            .await
            .is_err(),
        "deployment mismatch"
    );

    let mut wrong = fixture.lineage.clone();
    wrong
        .postgres_identity
        .as_mut()
        .expect("PostgreSQL identity")
        .system_identifier
        .0 ^= 1;
    assert!(
        attest_connection(&mut runtime, &fixture.with_lineage(wrong))
            .await
            .is_err(),
        "system mismatch"
    );

    let mut wrong = fixture.lineage.clone();
    wrong
        .postgres_identity
        .as_mut()
        .expect("PostgreSQL identity")
        .database
        .name = PgDatabaseName("wrong_database".to_owned());
    assert!(
        attest_connection(&mut runtime, &fixture.with_lineage(wrong))
            .await
            .is_err(),
        "database mismatch"
    );

    let mut wrong = fixture.lineage.clone();
    wrong
        .postgres_identity
        .as_mut()
        .expect("PostgreSQL identity")
        .schema
        .oid ^= 1;
    assert!(
        attest_connection(&mut runtime, &fixture.with_lineage(wrong))
            .await
            .is_err(),
        "schema mismatch"
    );

    let wrong_login_options = fixture.admin_options.clone();
    assert!(
        attested_pool(
            wrong_login_options,
            config.clone(),
            1,
            Duration::from_millis(350)
        )
        .is_err(),
        "configured login mismatch"
    );

    sqlx::query(&format!(
        "DROP FUNCTION {}.read_physical_identity()",
        fixture.trusted
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove attestation function");
    assert!(
        attest_connection(&mut runtime, &config).await.is_err(),
        "missing function"
    );
    drop(runtime);
    fixture.cleanup().await;
}

#[tokio::test]
async fn misplaced_admin_identity_is_rejected_before_ddl() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let mut wrong = fixture.lineage.clone();
    wrong
        .postgres_identity
        .as_mut()
        .expect("PostgreSQL identity")
        .database
        .oid ^= 1;
    assert!(install(&fixture.admin_options, fixture.with_lineage(wrong))
        .await
        .is_err());
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_catalog.pg_roles WHERE rolname = $1")
            .bind(&fixture.definer)
            .fetch_one(&fixture.admin)
            .await
            .expect("read role catalog after preflight denial");
    assert_eq!(remaining, 0, "preflight must precede CREATE ROLE");
    fixture.cleanup().await;
}

#[tokio::test]
async fn cancelled_attestation_hook_does_not_lend_a_connection() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let config = install(&fixture.admin_options, fixture.config())
        .await
        .expect("install attestation");
    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config,
        1,
        Duration::from_secs(3),
    )
    .expect("attested pool");

    let mut blocker = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("blocker connection");
    sqlx::query("BEGIN")
        .execute(&mut blocker)
        .await
        .expect("begin blocker");
    sqlx::query(&format!(
        "LOCK TABLE {}._lineage IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut blocker)
    .await
    .expect("block attestation read");

    let acquiring = {
        let pool = pool.clone();
        tokio::spawn(async move { pool.acquire().await.map(|_| ()) })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity \
             WHERE query LIKE '%read_physical_identity%' AND wait_event_type = 'Lock')",
        )
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect blocked attestation hook");
        if blocked {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "hook never reached lock"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !acquiring.is_finished(),
        "blocked hook cannot lend connection"
    );
    acquiring.abort();
    assert!(acquiring
        .await
        .expect_err("cancelled acquire")
        .is_cancelled());
    sqlx::query("ROLLBACK")
        .execute(&mut blocker)
        .await
        .expect("release attestation read");
    drop(blocker);
    let member = pool
        .acquire()
        .await
        .expect("replacement passes attestation");
    drop(member);
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn poisoned_idle_session_is_discarded_before_checkout() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let config = install(&fixture.admin_options, fixture.config())
        .await
        .expect("install attestation");
    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config,
        1,
        Duration::from_secs(2),
    )
    .expect("attested pool");

    let mut first = pool.acquire().await.expect("first attested checkout");
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *first)
        .await
        .expect("first backend PID");
    sqlx::query("SET search_path TO pg_catalog")
        .execute(&mut *first)
        .await
        .expect("poison session search path");
    drop(first);

    let mut second = pool
        .acquire()
        .await
        .expect("replacement after search path poison");
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *second)
        .await
        .expect("second backend PID");
    assert_ne!(second_pid, first_pid, "poisoned session must be discarded");
    let active_schema: String = sqlx::query_scalar("SELECT pg_catalog.current_schema()::text")
        .fetch_one(&mut *second)
        .await
        .expect("replacement active schema");
    assert_eq!(active_schema, fixture.target);

    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {} TO {}",
        fixture.target, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("outsider target schema usage");
    sqlx::query(&format!(
        "GRANT {} TO {}",
        fixture.outsider, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("allow test SET ROLE");
    sqlx::query(&format!("SET ROLE {}", fixture.outsider))
        .execute(&mut *second)
        .await
        .expect("poison session role");
    drop(second);

    let mut third = pool.acquire().await.expect("replacement after role poison");
    let third_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *third)
        .await
        .expect("third backend PID");
    assert_ne!(
        third_pid, second_pid,
        "role-poisoned session must be discarded"
    );
    let current_user: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut *third)
        .await
        .expect("replacement current user");
    assert_eq!(current_user, fixture.runtime);
    drop(third);
    pool.close().await;
    sqlx::query(&format!(
        "REVOKE {} FROM {}",
        fixture.outsider, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove test role membership");
    fixture.cleanup().await;
}

#[tokio::test]
async fn raw_repeatable_read_snapshot_cannot_attest_stale_lineage() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let config = install(&fixture.admin_options, fixture.config())
        .await
        .expect("install attestation");
    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config,
        1,
        Duration::from_millis(500),
    )
    .expect("attested pool");
    let mut member = pool.acquire().await.expect("initial attested checkout");
    let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *member)
        .await
        .expect("backend PID");
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *member)
        .await
        .expect("start raw repeatable-read transaction");
    let stored: String = sqlx::query_scalar(&format!(
        "SELECT lineage_uuid FROM {}.read_physical_identity()",
        fixture.trusted
    ))
    .fetch_one(&mut *member)
    .await
    .expect("pin old lineage snapshot");
    assert_eq!(stored, fixture.lineage.lineage_uuid.to_string());

    let replacement = uuid::Uuid::new_v4().to_string();
    let updated = sqlx::query(&format!(
        "UPDATE {}._lineage SET lineage_uuid = $1 WHERE id = 1",
        fixture.target
    ))
    .bind(&replacement)
    .execute(&fixture.admin)
    .await
    .expect("commit changed lineage outside pinned snapshot");
    assert_eq!(updated.rows_affected(), 1);
    drop(member);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let state: Option<String> =
            sqlx::query_scalar("SELECT state FROM pg_catalog.pg_stat_activity WHERE pid = $1")
                .bind(pid)
                .fetch_optional(&fixture.admin)
                .await
                .expect("inspect returned backend");
        if pool.num_idle() == 1 && state.as_deref() == Some("idle in transaction") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "raw transaction was not returned to the idle pool"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        pool.acquire().await.is_err(),
        "old snapshot must not attest after committed lineage change"
    );

    sqlx::query(&format!(
        "UPDATE {}._lineage SET lineage_uuid = $1 WHERE id = 1",
        fixture.target
    ))
    .bind(fixture.lineage.lineage_uuid.to_string())
    .execute(&fixture.admin)
    .await
    .expect("restore fixture lineage");
    let replacement_member = pool.acquire().await.expect("restored lineage attests");
    drop(replacement_member);
    pool.close().await;
    fixture.cleanup().await;
}

fn url_with_schema(base: &str, schema: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
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

fn test_url() -> Option<String> {
    waddle_ws_test_support::require_test_postgres_url()
}

#[tokio::test]
async fn committed_function_attests_every_physical_connection_and_replacement() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let config = install(&fixture.admin_options, fixture.config())
        .await
        .expect("committed installer and fresh-session readback");

    // A role with schema usage but no function EXECUTE must not be able to
    // read another installation's identity through PUBLIC's default grant.
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {} TO {}",
        fixture.trusted, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("outsider schema usage");
    let outsider_options = fixture.admin_options.clone().username(&fixture.outsider);
    let mut outsider = PgConnection::connect_with(&outsider_options)
        .await
        .expect("outsider login");
    let outsider_read = sqlx::query(&format!(
        "SELECT * FROM {}.read_physical_identity()",
        fixture.trusted
    ))
    .fetch_one(&mut outsider)
    .await;
    assert!(
        outsider_read.is_err(),
        "PUBLIC must have no function EXECUTE"
    );
    drop(outsider);

    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        2,
        Duration::from_secs(2),
    )
    .expect("pool configuration");
    let mut first = pool.acquire().await.expect("first attested connection");
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *first)
        .await
        .expect("first backend PID");
    let mut second = pool.acquire().await.expect("expanded attested connection");
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *second)
        .await
        .expect("second backend PID");
    assert_ne!(
        first_pid, second_pid,
        "pool expansion needs a physical connection"
    );
    drop(first);

    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(first_pid)
        .fetch_one(&fixture.admin)
        .await
        .expect("terminate one backend");
    assert!(terminated);
    let mut replacement = pool
        .acquire()
        .await
        .expect("replacement attested connection");
    let replacement_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *replacement)
        .await
        .expect("replacement backend PID");
    assert_ne!(replacement_pid, first_pid);
    assert_ne!(
        replacement_pid, second_pid,
        "surviving backend remains checked out during replacement"
    );
    drop(replacement);
    drop(second);
    pool.close().await;

    // The same check rejects a privileged login even if its DSN can reach
    // the function; configured username alone does not establish identity.
    let mut admin_connection = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("fresh admin connection");
    assert!(attest_connection(&mut admin_connection, &config)
        .await
        .is_err());
    drop(admin_connection);

    fixture.cleanup().await;
}

#[tokio::test]
async fn wrong_identity_revoked_grant_and_spoofed_schema_never_lend_a_connection() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let config = install(&fixture.admin_options, fixture.config())
        .await
        .expect("install attestation");

    let mut wrong = fixture.lineage.clone();
    wrong.lineage_uuid = LineageUuid::new();
    let denied = attested_pool(
        fixture.runtime_options.clone(),
        fixture.with_lineage(wrong),
        1,
        Duration::from_millis(350),
    )
    .expect("wrong-lineage pool is constructible");
    assert!(
        denied.acquire().await.is_err(),
        "wrong lineage cannot be lent"
    );
    denied.close().await;

    let spoof = format!("waddle_attest_s_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {spoof}"))
        .execute(&fixture.admin)
        .await
        .expect("spoof schema");
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {spoof} TO {}",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("spoof schema usage");
    sqlx::query(&format!(
        "CREATE FUNCTION {spoof}.current_schema() RETURNS text \
         LANGUAGE sql AS $$ SELECT '{}'::text $$",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("shadow catalog current_schema");
    let spoof_url = url_with_schema(&fixture.base_url, &format!("{spoof},pg_catalog"));
    let spoof_options = PgConnectOptions::from_str(&spoof_url)
        .expect("spoof options")
        .username(&fixture.runtime);
    let mut spoof_connection = PgConnection::connect_with(&spoof_options)
        .await
        .expect("spoof connection");
    let shadowed_schema: String = sqlx::query_scalar("SELECT current_schema()::text")
        .fetch_one(&mut spoof_connection)
        .await
        .expect("unqualified function is shadowed");
    let actual_schema: String = sqlx::query_scalar("SELECT pg_catalog.current_schema()::text")
        .fetch_one(&mut spoof_connection)
        .await
        .expect("qualified catalog function");
    assert_eq!(shadowed_schema, fixture.target);
    assert_eq!(actual_schema, spoof);
    drop(spoof_connection);
    let spoofed = attested_pool(spoof_options, config.clone(), 1, Duration::from_millis(350))
        .expect("spoof pool is constructible");
    assert!(
        spoofed.acquire().await.is_err(),
        "spoofed search_path cannot be lent"
    );
    spoofed.close().await;
    sqlx::query(&format!("DROP SCHEMA {spoof} CASCADE"))
        .execute(&fixture.admin)
        .await
        .expect("drop spoof schema");

    sqlx::query(&format!(
        "REVOKE EXECUTE ON FUNCTION {}.read_physical_identity() FROM {}",
        fixture.trusted, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke runtime execution");
    let revoked = attested_pool(
        fixture.runtime_options.clone(),
        config,
        1,
        Duration::from_millis(350),
    )
    .expect("revoked pool is constructible");
    assert!(
        revoked.acquire().await.is_err(),
        "revoked function cannot be lent"
    );
    revoked.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn failed_install_rolls_back_the_definer_role() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    sqlx::query(&format!("CREATE SCHEMA {}", fixture.trusted))
        .execute(&fixture.admin)
        .await
        .expect("pre-existing trusted schema collision");
    assert!(install(&fixture.admin_options, fixture.config())
        .await
        .is_err());
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_catalog.pg_roles WHERE rolname = $1")
            .bind(&fixture.definer)
            .fetch_one(&fixture.admin)
            .await
            .expect("read role catalog after failed installer");
    assert_eq!(
        remaining, 0,
        "CREATE ROLE must roll back with the installer"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn inherited_outsider_function_grant_aborts_before_commit() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    sqlx::query(&format!(
        "ALTER DEFAULT PRIVILEGES GRANT EXECUTE ON FUNCTIONS TO {}",
        fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("seed administrator default function grant");

    let error = install(&fixture.admin_options, fixture.config())
        .await
        .expect_err("outsider default grant must abort installer");
    assert!(
        matches!(
            &error,
            PhysicalAttestationError::Mismatch {
                field: "function ACL grantees"
            }
        ),
        "unexpected grant must be detected before COMMIT: {error:?}"
    );
    let role_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_catalog.pg_roles WHERE rolname = $1")
            .bind(&fixture.definer)
            .fetch_one(&fixture.admin)
            .await
            .expect("read rolled-back role");
    let schema_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_catalog.pg_namespace WHERE nspname = $1")
            .bind(&fixture.trusted)
            .fetch_one(&fixture.admin)
            .await
            .expect("read rolled-back schema");
    assert_eq!(role_count, 0, "definer role must roll back");
    assert_eq!(schema_count, 0, "trusted schema/function must roll back");

    sqlx::query(&format!(
        "ALTER DEFAULT PRIVILEGES REVOKE EXECUTE ON FUNCTIONS FROM {}",
        fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove administrator default function grant");
    fixture.cleanup().await;
}

#[tokio::test]
async fn inherited_runtime_grant_option_aborts_before_commit() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    sqlx::query(&format!(
        "ALTER DEFAULT PRIVILEGES GRANT EXECUTE ON FUNCTIONS TO {} WITH GRANT OPTION",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("seed runtime default grant option");

    let error = install(&fixture.admin_options, fixture.config())
        .await
        .expect_err("runtime grant option must abort installer");
    assert!(
        matches!(
            &error,
            PhysicalAttestationError::Mismatch {
                field: "function ACL grantees"
            }
        ),
        "runtime grant option must be detected before COMMIT: {error:?}"
    );
    let role_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_catalog.pg_roles WHERE rolname = $1")
            .bind(&fixture.definer)
            .fetch_one(&fixture.admin)
            .await
            .expect("read rolled-back definer role");
    assert_eq!(role_count, 0, "definer role must roll back");

    sqlx::query(&format!(
        "ALTER DEFAULT PRIVILEGES REVOKE EXECUTE ON FUNCTIONS FROM {}",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove runtime default grant option");
    fixture.cleanup().await;
}
