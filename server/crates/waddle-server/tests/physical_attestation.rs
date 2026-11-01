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
    attest_connection, attested_pool, inspect_installation, install, DarkIngressAuthority,
    DarkIngressAuthorityError, InstallationStatus, PgRoleName, PhysicalAttestationConfig,
    PhysicalAttestationError, TransactionBounds,
};
#[cfg(feature = "test-support")]
use waddle_server::db::MigrationRunner;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::ingress::EffectReceiptKey;
use waddle_server::ingress_substrate::EffectReceiptKind;
#[cfg(feature = "test-support")]
use waddle_server::ingress_substrate::IngressSubstrateError;
use waddle_xmpp::ingress::{MessageKey, ProtocolEpoch};

// Every installer edits pg_control_system's catalog ACL. Cargo test runs
// cases in this binary concurrently; keep those DDL transactions distinct.
static CATALOG_GRANT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    cluster_admin: PgPool,
    admin: PgPool,
    admin_options: PgConnectOptions,
    installer_options: PgConnectOptions,
    runtime_options: PgConnectOptions,
    base_url: String,
    database_name: String,
    lineage: AttestedLineage,
    target: String,
    trusted: String,
    definer: String,
    installer: String,
    runtime: String,
    outsider: String,
    operation_id: uuid::Uuid,
}

impl Fixture {
    async fn new(url: String) -> Self {
        let cluster_admin = PgPool::connect(&url).await.expect("admin PostgreSQL pool");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database_name = format!("attest_db_{suffix}");
        let target = format!("waddle_attest_t_{suffix}");
        let trusted = format!("waddle_attest_f_{suffix}");
        let definer = format!("waddle_attest_d_{suffix}");
        let installer = format!("waddle_attest_i_{suffix}");
        let runtime = format!("waddle_attest_r_{suffix}");
        let outsider = format!("waddle_attest_o_{suffix}");
        sqlx::query(&format!("CREATE DATABASE {database_name}"))
            .execute(&cluster_admin)
            .await
            .expect("isolated test database");
        sqlx::query(&format!(
            "REVOKE TEMPORARY ON DATABASE {database_name} FROM PUBLIC"
        ))
        .execute(&cluster_admin)
        .await
        .expect("revoke public TEMP in isolated database");
        sqlx::query(&format!("CREATE ROLE {definer} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"))
            .execute(&cluster_admin).await.expect("preprovision definer");
        for role in [&runtime, &outsider, &installer] {
            sqlx::query(&format!("CREATE ROLE {role} LOGIN"))
                .execute(&cluster_admin)
                .await
                .expect("test login role");
        }
        sqlx::query(&format!(
            "GRANT {definer} TO {installer} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"
        ))
        .execute(&cluster_admin)
        .await
        .expect("grant restricted installer role handoff");
        let base_url = url_with_database(&url, &database_name);
        let admin = PgPool::connect(&base_url)
            .await
            .expect("isolated database administrator");
        sqlx::query("REVOKE ALL ON SCHEMA public FROM PUBLIC")
            .execute(&admin)
            .await
            .expect("restrict public schema");
        sqlx::query(&format!("CREATE SCHEMA {target}"))
            .execute(&admin)
            .await
            .expect("target schema");
        sqlx::query(&format!("CREATE SCHEMA {trusted} AUTHORIZATION {definer}"))
            .execute(&admin)
            .await
            .expect("preprovision trusted schema");
        sqlx::query(&format!("GRANT USAGE ON SCHEMA {target} TO {runtime}"))
            .execute(&admin)
            .await
            .expect("runtime schema usage");

        let boundary_url = url_with_schema(&base_url, &target);
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
        for statement in [
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC".to_owned(),
            format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {definer}"),
            format!("GRANT USAGE ON SCHEMA {target} TO {definer}"),
            format!("GRANT SELECT ON {target}._lineage TO {definer}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("preprovision definer grant");
        }

        let admin_options = PgConnectOptions::from_str(&boundary_url).expect("admin options");
        let installer_options = admin_options.clone().username(&installer);
        let runtime_options = admin_options.clone().username(&runtime);
        Self {
            cluster_admin,
            admin,
            admin_options,
            installer_options,
            runtime_options,
            base_url,
            database_name,
            lineage,
            target,
            trusted,
            definer,
            installer,
            runtime,
            outsider,
            operation_id: uuid::Uuid::new_v4(),
        }
    }

    fn config(&self) -> PhysicalAttestationConfig {
        self.with_lineage(self.lineage.clone())
    }

    fn with_lineage(&self, lineage: AttestedLineage) -> PhysicalAttestationConfig {
        PhysicalAttestationConfig::new(
            lineage,
            PgRoleName::new(self.installer.clone()).expect("test installer role"),
            PgRoleName::new(self.runtime.clone()).expect("test runtime role"),
            PgRoleName::new(self.definer.clone()).expect("test definer role"),
            PgSchemaName(self.trusted.clone()),
        )
        .expect("valid attestation configuration")
    }

    async fn cleanup(self) {
        self.admin.close().await;
        sqlx::query(&format!(
            "DROP DATABASE {} WITH (FORCE)",
            self.database_name
        ))
        .execute(&self.cluster_admin)
        .await
        .expect("drop isolated test database");
        for role in [
            &self.runtime,
            &self.outsider,
            &self.installer,
            &self.definer,
        ] {
            sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
                .execute(&self.cluster_admin)
                .await
                .expect("drop test role");
        }
        self.cluster_admin.close().await;
    }
}

async fn typed_authority_fixture(fixture: &Fixture) -> (PhysicalAttestationConfig, MessageKey) {
    let url = url_with_schema(&fixture.base_url, &fixture.target);
    let db = Database::from_config(
        "typed authority schema",
        &DatabaseConfig::new(DatabaseDriver::Postgres, url),
    )
    .await
    .expect("open migration database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("apply actual migrations");
    drop(db);

    let key = MessageKey::from_storage(uuid::Uuid::new_v4());
    let mut owner = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("connect schema owner");
    sqlx::query(&format!(
        "INSERT INTO {}.ingress_messages (message_key, digest_version, digest) VALUES ($1, 1, $2)",
        fixture.target
    ))
    .bind(key.to_storage())
    .bind([0_u8; 32].as_slice())
    .execute(&mut owner)
    .await
    .expect("seed canonical message");
    sqlx::query(&format!(
        "INSERT INTO {}.ingress_effect_intents (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES ($1, 0, 1, $2, 1, $3)",
        fixture.target
    ))
    .bind(key.to_storage())
    .bind([1_u8; 32].as_slice())
    .bind(Vec::<u8>::new())
    .execute(&mut owner)
    .await
    .expect("seed receipt intent");
    sqlx::raw_sql(include_str!(
        "fixtures/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&mut owner)
    .await
    .expect("install laboratory-only guard");
    for statement in [
        format!(
            "GRANT SELECT ON {}.ingress_protocol_epoch TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT SELECT, INSERT ON {}.ingress_effect_receipts TO {}",
            fixture.target, fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&mut owner)
            .await
            .expect("grant restricted runtime");
    }
    drop(owner);
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install physical attestation");
    (config, key)
}

fn typed_bounds() -> TransactionBounds {
    TransactionBounds::new(
        Duration::from_secs(5),
        Duration::from_secs(3),
        Duration::from_secs(4),
    )
    .expect("valid bounds")
}

fn typed_receipt_key() -> EffectReceiptKey {
    EffectReceiptKey {
        kind: EffectReceiptKind::from_storage(1),
        semantic_identity_hash: [1_u8; 32],
    }
}

async fn wait_for_blocked_application(admin: &PgPool, application: &str, blocker: i32) -> i32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiter: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_stat_activity \
                 WHERE application_name = $1 AND $2 = ANY(pg_catalog.pg_blocking_pids(pid))",
            )
            .bind(application)
            .bind(blocker)
            .fetch_optional(admin)
            .await
            .expect("observe identified waiter");
            if let Some(pid) = waiter {
                break pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("identified database waiter")
}

#[tokio::test]
async fn typed_authority_waits_before_read_and_rejects_future_epoch() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, _) = typed_authority_fixture(&fixture).await;
    let application = format!("typed_epoch_{}", uuid::Uuid::new_v4().simple());
    let options = fixture
        .runtime_options
        .clone()
        .application_name(&application)
        .options([("default_transaction_isolation", "repeatable\\ read")]);
    let authority = std::sync::Arc::new(
        DarkIngressAuthority::postgres(options, config, 1, typed_bounds())
            .expect("dark authority configuration"),
    );
    let mut transition = fixture.admin.begin().await.expect("owner transaction");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *transition)
        .await
        .expect("owner isolation");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *transition)
    .await
    .expect("owner first lock");
    sqlx::query(&format!(
        "UPDATE {}.ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = $1 WHERE id = 1",
        fixture.target
    ))
    .bind(fixture.lineage.lineage_uuid.0)
    .execute(&mut *transition)
    .await
    .expect("advance epoch");
    let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *transition)
        .await
        .expect("owner PID");
    let worker = {
        let authority = authority.clone();
        tokio::spawn(async move { authority.current_epoch().await })
    };
    wait_for_blocked_application(&fixture.admin, &application, blocker).await;
    transition.commit().await.expect("commit transition");
    assert_eq!(
        worker.await.expect("worker").expect("current epoch"),
        ProtocolEpoch::from_storage(1)
    );

    let mut transition = fixture.admin.begin().await.expect("second transition");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *transition)
    .await
    .expect("second owner first lock");
    sqlx::query(&format!(
        "UPDATE {}.ingress_protocol_epoch SET epoch = 2 WHERE id = 1",
        fixture.target
    ))
    .execute(&mut *transition)
    .await
    .expect("future epoch");
    transition.commit().await.expect("commit future epoch");
    assert!(matches!(
        authority.current_epoch().await,
        Err(DarkIngressAuthorityError::UnsupportedEpoch)
    ));
    drop(authority);
    fixture.cleanup().await;
}

#[tokio::test]
async fn typed_receipt_holds_relation_without_singleton_row_lock() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, key) = typed_authority_fixture(&fixture).await;
    let application = format!("typed_receipt_{}", uuid::Uuid::new_v4().simple());
    let options = fixture
        .runtime_options
        .clone()
        .application_name(&application);
    let authority = std::sync::Arc::new(
        DarkIngressAuthority::postgres(options, config, 1, typed_bounds())
            .expect("dark authority configuration"),
    );
    let mut parent = fixture.admin.begin().await.expect("lock intent parent");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_effect_intents WHERE message_key = $1 FOR UPDATE",
        fixture.target
    ))
    .bind(key.to_storage())
    .fetch_one(&mut *parent)
    .await
    .expect("hold intent row");
    let parent_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *parent)
        .await
        .expect("parent PID");
    let worker = {
        let authority = authority.clone();
        tokio::spawn(async move { authority.record_receipt(key, &typed_receipt_key()).await })
    };
    let writer = wait_for_blocked_application(&fixture.admin, &application, parent_pid).await;
    let (share, row_share): (bool, bool) = sqlx::query_as(
        "SELECT bool_or(mode = 'AccessShareLock'), bool_or(mode = 'RowShareLock') \
         FROM pg_catalog.pg_locks WHERE pid = $1 AND relation = $2::regclass AND granted",
    )
    .bind(writer)
    .bind(format!("{}.ingress_protocol_epoch", fixture.target))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect epoch relation locks");
    assert!(share, "writer must hold the relation prelock");
    assert!(!row_share, "writer must not request FOR SHARE on the epoch");
    parent.commit().await.expect("release parent");
    worker.await.expect("receipt task").expect("receipt commit");
    authority
        .record_receipt(key, &typed_receipt_key())
        .await
        .expect("receipt retry is idempotent");
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.ingress_effect_receipts WHERE message_key = $1",
        fixture.target
    ))
    .bind(key.to_storage())
    .fetch_one(&fixture.admin)
    .await
    .expect("read receipt");
    assert_eq!(count, 1);
    drop(authority);
    fixture.cleanup().await;
}

#[tokio::test]
async fn typed_authority_fails_closed_on_identity_epoch_and_lock_timeout() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, _) = typed_authority_fixture(&fixture).await;
    let short_bounds = TransactionBounds::new(
        Duration::from_secs(2),
        Duration::from_millis(100),
        Duration::from_secs(1),
    )
    .expect("short bounds");
    let authority =
        DarkIngressAuthority::postgres(fixture.runtime_options.clone(), config, 1, short_bounds)
            .expect("dark authority configuration");
    let mut owner = fixture.admin.begin().await.expect("owner transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *owner)
    .await
    .expect("block relation lock");
    let blocked = tokio::time::timeout(Duration::from_secs(1), authority.current_epoch())
        .await
        .expect("relation lock timeout precedes the two-second acquisition deadline");
    assert!(matches!(blocked, Err(DarkIngressAuthorityError::Timeout)));
    owner.rollback().await.expect("release relation lock");
    assert_eq!(
        authority
            .current_epoch()
            .await
            .expect("connection reusable"),
        ProtocolEpoch::ZERO
    );

    let mut wrong = fixture.lineage.clone();
    let wrong_schema = &mut wrong
        .postgres_identity
        .as_mut()
        .expect("PostgreSQL identity")
        .schema;
    wrong_schema.oid = wrong_schema.oid.wrapping_add(1);
    let mismatched = DarkIngressAuthority::postgres(
        fixture.runtime_options.clone(),
        fixture.with_lineage(wrong),
        1,
        TransactionBounds::new(
            Duration::from_millis(500),
            Duration::from_millis(100),
            Duration::from_millis(250),
        )
        .expect("identity bounds"),
    )
    .expect("construct mismatched authority");
    assert!(
        mismatched.current_epoch().await.is_err(),
        "a wrong expected schema OID must never lend a transaction"
    );
    drop(mismatched);

    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch DISABLE TRIGGER ingress_protocol_epoch_guard_row",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("model missing epoch row");
    sqlx::query(&format!(
        "DELETE FROM {}.ingress_protocol_epoch WHERE id = 1",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove singleton epoch row");
    assert!(matches!(
        authority.current_epoch().await,
        Err(DarkIngressAuthorityError::MissingEpoch)
    ));
    drop(authority);
    fixture.cleanup().await;
}

#[tokio::test]
async fn typed_sqlite_authority_uses_shared_foundation_and_immediate_transaction() {
    let url = format!(
        "sqlite:file:typed_authority_{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4().simple()
    );
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, &url);
    config.pool_size = 1;
    let db = Database::from_config("typed sqlite owner", &config)
        .await
        .expect("open shared memory database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("SQLite migrations");
    let key = MessageKey::from_storage(uuid::Uuid::new_v4());
    let conn = db.guard().await.expect("owner guard");
    conn.execute(
        "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES (?, 1, ?)",
        waddle_server::db_params![key.to_storage().to_string(), vec![0_u8; 32]],
    )
    .await
    .expect("seed message");
    conn.execute(
        "INSERT INTO ingress_effect_intents (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES (?, '0', 1, ?, 1, ?)",
        waddle_server::db_params![key.to_storage().to_string(), vec![1_u8; 32], Vec::<u8>::new()],
    )
    .await
    .expect("seed intent");
    drop(conn);
    let sqlite_bounds = TransactionBounds::new(
        Duration::from_millis(250),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .expect("SQLite bounds");
    let authority = DarkIngressAuthority::sqlite(&url, sqlite_bounds)
        .await
        .expect("open shared SQLite authority");
    assert_eq!(
        authority.current_epoch().await.expect("epoch"),
        ProtocolEpoch::ZERO
    );
    authority
        .record_receipt(key, &typed_receipt_key())
        .await
        .expect("receipt");
    authority
        .record_receipt(key, &typed_receipt_key())
        .await
        .expect("idempotent receipt");
    let conn = db.guard().await.expect("read guard");
    let mut rows = conn
        .query("SELECT count(*) FROM ingress_effect_receipts", ())
        .await
        .expect("receipt query");
    let count: i64 = rows
        .next()
        .await
        .expect("row read")
        .expect("row")
        .get(0)
        .expect("count");
    assert_eq!(count, 1);

    let holder = db.begin_immediate().await.expect("reserve SQLite writer");
    let blocked = tokio::time::timeout(Duration::from_secs(1), authority.current_epoch())
        .await
        .expect("immediate acquisition is bounded");
    assert!(
        blocked.is_err(),
        "BEGIN IMMEDIATE must respect the existing writer"
    );
    holder.rollback().await.expect("release SQLite writer");
    assert_eq!(
        authority
            .current_epoch()
            .await
            .expect("SQLite handle remains usable"),
        ProtocolEpoch::ZERO
    );

    let absent = DarkIngressAuthority::sqlite("sqlite::memory:", typed_bounds())
        .await
        .expect("open empty private SQLite database");
    assert!(
        absent.current_epoch().await.is_err(),
        "an absent foundation must not mint a transaction"
    );
}

#[tokio::test]
async fn identity_variants_missing_function_and_login_mismatch_fail_closed() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
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

    sqlx::query(&format!(
        "UPDATE {}._lineage SET format = 2",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("change only the lineage format");
    let function_rows: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}.read_physical_identity()",
        fixture.trusted
    ))
    .fetch_one(&mut runtime)
    .await
    .expect("call installed function after format change");
    assert_eq!(function_rows, 0, "installed function must reject format 2");
    assert!(matches!(
        attest_connection(&mut runtime, &config).await,
        Err(PhysicalAttestationError::MissingLineage)
    ));
    sqlx::query(&format!(
        "UPDATE {}._lineage SET format = 1",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore lineage format");

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
    assert!(install(
        &fixture.installer_options,
        fixture.with_lineage(wrong),
        fixture.operation_id
    )
    .await
    .is_err());
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.proname = 'read_physical_identity'")
            .bind(&fixture.trusted)
            .fetch_one(&fixture.admin)
            .await
            .expect("read function catalog after preflight denial");
    assert_eq!(remaining, 0, "preflight must precede function creation");
    sqlx::query(&format!(
        "UPDATE {}._lineage SET format = 2",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("change only the lineage format");
    assert!(matches!(
        install(
            &fixture.installer_options,
            fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::Mismatch {
            field: "lineage format"
        })
    ));
    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.proname = 'read_physical_identity'",
    )
            .bind(&fixture.trusted)
            .fetch_one(&fixture.admin)
            .await
            .expect("read function catalog after format denial");
    assert_eq!(
        remaining, 0,
        "format preflight must precede function creation"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn cancelled_attestation_hook_does_not_lend_a_connection() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
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
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
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
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
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

fn url_with_database(base: &str, database: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "dbname" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("dbname", database);
    url.to_string()
}

fn url_with_schema(base: &str, schema: &str) -> String {
    url_with_search_path(base, schema)
}

fn url_with_search_path(base: &str, search_path: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={search_path}"));
    url.to_string()
}

fn test_url() -> Option<String> {
    waddle_ws_test_support::require_test_postgres_url()
}

#[cfg(feature = "test-support")]
async fn prepare_attested_writer_fixture(fixture: &Fixture) -> (String, PhysicalAttestationConfig) {
    let boundary_url = url_with_schema(&fixture.base_url, &fixture.target);
    let database = Database::from_config(
        "attested-writer-migrations",
        &DatabaseConfig::new(DatabaseDriver::Postgres, boundary_url.clone()),
    )
    .await
    .expect("migration database");
    MigrationRunner::single()
        .run(&database)
        .await
        .expect("install ingress migrations including V1009");
    drop(database);

    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install physical attestor after lineage and schema migrations");
    for statement in [
        format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {} TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA {} TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT SELECT ON {}._lineage TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT TEMPORARY ON DATABASE {} TO {}",
            fixture.database_name, fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("grant disposable writer fixture privileges");
    }
    (boundary_url, config)
}

#[cfg(feature = "test-support")]
async fn ingress_message_count(pool: &PgPool, target: &str) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM \"{target}\".ingress_messages"
    ))
    .fetch_one(pool)
    .await
    .expect("count guarded ingress messages")
}

#[cfg(feature = "test-support")]
async fn wait_for_blocking_pid(observer: &PgPool, waiter: i32, blocker: i32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let blocked: bool =
                sqlx::query_scalar("SELECT $1 = ANY(pg_catalog.pg_blocking_pids($2))")
                    .bind(blocker)
                    .bind(waiter)
                    .fetch_one(observer)
                    .await
                    .expect("observe blocked epoch transition");
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("identify row-share lock wait");
}

#[tokio::test]
async fn committed_function_attests_every_physical_connection_and_replacement() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
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
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
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
async fn failed_install_preserves_preprovisioned_role_and_existing_function() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    sqlx::query(&format!(
        "CREATE FUNCTION {}.read_physical_identity() RETURNS integer LANGUAGE sql AS 'SELECT 42'",
        fixture.trusted
    ))
    .execute(&fixture.admin)
    .await
    .expect("pre-existing function collision");
    assert!(matches!(
        install(
            &fixture.installer_options,
            fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::InstallationConflict)
    ));
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_catalog.pg_roles WHERE rolname = $1")
            .bind(&fixture.definer)
            .fetch_one(&fixture.admin)
            .await
            .expect("read role catalog after failed installer");
    assert_eq!(remaining, 1, "preprovisioned role must remain");
    let source: String = sqlx::query_scalar("SELECT p.prosrc FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.proname = 'read_physical_identity'")
        .bind(&fixture.trusted).fetch_one(&fixture.admin).await.expect("existing function source");
    assert_eq!(source, "SELECT 42");
    fixture.cleanup().await;
}

#[tokio::test]
async fn inherited_outsider_function_grant_aborts_before_commit() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    sqlx::query(&format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {} GRANT EXECUTE ON FUNCTIONS TO {}",
        fixture.definer, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("seed administrator default function grant");

    let error = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect_err("outsider default grant must abort installer");
    assert!(
        matches!(&error, PhysicalAttestationError::InstallationConflict),
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
    assert_eq!(role_count, 1, "preprovisioned definer must remain");
    assert_eq!(schema_count, 1, "preprovisioned schema must remain");
    let functions: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname=$1 AND p.proname='read_physical_identity'").bind(&fixture.trusted).fetch_one(&fixture.admin).await.expect("rolled-back function");
    assert_eq!(functions, 0);

    sqlx::query(&format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {} REVOKE EXECUTE ON FUNCTIONS FROM {}",
        fixture.definer, fixture.outsider
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
        "ALTER DEFAULT PRIVILEGES FOR ROLE {} GRANT EXECUTE ON FUNCTIONS TO {} WITH GRANT OPTION",
        fixture.definer, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("seed runtime default grant option");

    let error = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect_err("runtime grant option must abort installer");
    assert!(
        matches!(&error, PhysicalAttestationError::InstallationConflict),
        "runtime grant option must be detected before COMMIT: {error:?}"
    );
    let role_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_catalog.pg_roles WHERE rolname = $1")
            .bind(&fixture.definer)
            .fetch_one(&fixture.admin)
            .await
            .expect("read rolled-back definer role");
    assert_eq!(role_count, 1, "preprovisioned definer must remain");

    sqlx::query(&format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {} REVOKE EXECUTE ON FUNCTIONS FROM {}",
        fixture.definer, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove runtime default grant option");
    fixture.cleanup().await;
}

#[tokio::test]
async fn same_operation_retry_verifies_committed_install_and_rejects_conflicts() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let release_original = std::sync::Arc::new(tokio::sync::Notify::new());
    let gate = release_original.clone();
    let original_options = fixture.installer_options.clone();
    let original_config = fixture.config();
    let original_id = fixture.operation_id;
    let original = tokio::spawn(async move {
        gate.notified().await;
        install(&original_options, original_config, original_id).await
    });
    assert_eq!(
        inspect_installation(
            &fixture.installer_options,
            &fixture.config(),
            fixture.operation_id
        )
        .await
        .expect("inspect absent installation"),
        InstallationStatus::Indeterminate
    );
    install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install with restricted login");
    release_original.notify_one();
    original
        .await
        .expect("join delayed original request")
        .expect("delayed same-ID original must verify and converge");
    assert_eq!(
        inspect_installation(
            &fixture.installer_options,
            &fixture.config(),
            fixture.operation_id
        )
        .await
        .expect("inspect committed installation"),
        InstallationStatus::Committed
    );
    install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("same operation retries without replacement");
    sqlx::query(&format!(
        "ALTER FUNCTION {}.read_physical_identity() SET statement_timeout = '1s'",
        fixture.trusted
    ))
    .execute(&fixture.admin)
    .await
    .expect("add unexpected function setting");
    assert!(matches!(
        inspect_installation(
            &fixture.installer_options,
            &fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::InstallationConflict)
    ));
    sqlx::query(&format!(
        "ALTER FUNCTION {}.read_physical_identity() RESET statement_timeout",
        fixture.trusted
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore exact function settings");
    let other = uuid::Uuid::new_v4();
    assert!(matches!(
        install(&fixture.installer_options, fixture.config(), other).await,
        Err(PhysicalAttestationError::InstallationConflict)
    ));
    assert!(matches!(
        inspect_installation(&fixture.installer_options, &fixture.config(), other).await,
        Err(PhysicalAttestationError::InstallationConflict)
    ));
    let mut different_lineage = fixture.lineage.clone();
    different_lineage.deployment_uuid = DeploymentUuid(uuid::Uuid::new_v4());
    assert!(matches!(
        install(
            &fixture.installer_options,
            fixture.with_lineage(different_lineage),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::Mismatch {
            field: "deployment UUID"
        })
    ));
    assert!(matches!(
        install(
            &fixture.admin_options,
            fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::WrongInstallerLogin)
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn role_graph_and_effective_privilege_drift_abort_before_install() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let startup_role = fixture
        .installer_options
        .clone()
        .options([("role", fixture.definer.as_str())]);
    assert!(matches!(
        install(&startup_role, fixture.config(), fixture.operation_id).await,
        Err(PhysicalAttestationError::Mismatch {
            field: "installer login/session role"
        })
    ));
    sqlx::query(&format!(
        "GRANT {} TO {}",
        fixture.runtime, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("make outsider a runtime member");
    assert!(matches!(
        install(
            &fixture.installer_options,
            fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::Mismatch {
            field: "role membership"
        })
    ));
    sqlx::query(&format!(
        "REVOKE {} FROM {}",
        fixture.runtime, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke outsider membership");
    sqlx::query(&format!(
        "GRANT {} TO {}",
        fixture.definer, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant intermediary definer membership");
    sqlx::query(&format!(
        "GRANT {} TO {}",
        fixture.outsider, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant runtime intermediary membership");
    assert!(matches!(
        install(
            &fixture.installer_options,
            fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::Mismatch {
            field: "role membership"
        })
    ));
    sqlx::query(&format!(
        "REVOKE {} FROM {}",
        fixture.outsider, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke runtime intermediary");
    sqlx::query(&format!(
        "REVOKE {} FROM {}",
        fixture.definer, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke intermediary definer");
    sqlx::query(&format!(
        "GRANT UPDATE(lineage_uuid) ON {}._lineage TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant a single lineage column");
    assert!(matches!(
        install(
            &fixture.installer_options,
            fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::Mismatch {
            field: "runtime lineage modification privileges"
        })
    ));
    sqlx::query(&format!(
        "REVOKE UPDATE(lineage_uuid) ON {}._lineage FROM {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke lineage column");
    sqlx::query(&format!(
        "GRANT TEMPORARY ON DATABASE {} TO {}",
        fixture.database_name, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant TEMP to runtime");
    assert!(matches!(
        install(
            &fixture.installer_options,
            fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::Mismatch {
            field: "installer, runtime or definer CREATE/TEMP"
        })
    ));
    sqlx::query(&format!(
        "REVOKE TEMPORARY ON DATABASE {} FROM {}",
        fixture.database_name, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke TEMP");
    sqlx::query(&format!(
        "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {}",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant runtime direct control access");
    assert!(matches!(
        install(
            &fixture.installer_options,
            fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::Mismatch {
            field: "runtime control function privileges"
        })
    ));
    sqlx::query(&format!(
        "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {}",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke runtime direct control access");
    let functions: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname=$1 AND p.proname='read_physical_identity'")
        .bind(&fixture.trusted).fetch_one(&fixture.admin).await.expect("function count");
    assert_eq!(
        functions, 0,
        "failed prerequisite audits cannot install a function"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn spoofed_existing_function_is_rejected_without_invocation() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install original function");
    sqlx::query(&format!(
        "CREATE TABLE {}.invocation_probe (id int)",
        fixture.trusted
    ))
    .execute(&fixture.admin)
    .await
    .expect("create probe");
    sqlx::query(&format!(
        "ALTER TABLE {}.invocation_probe OWNER TO {}",
        fixture.trusted, fixture.definer
    ))
    .execute(&fixture.admin)
    .await
    .expect("give definer probe");
    let malicious = format!(
        r#"CREATE OR REPLACE FUNCTION {}.read_physical_identity()
RETURNS TABLE (
    lineage_uuid text, deployment_uuid text,
    stored_system_identifier text, stored_database_oid text, stored_database_name text,
    stored_schema_oid text, stored_schema_name text,
    live_system_identifier text, live_database_oid text, live_database_name text,
    live_schema_oid text, live_schema_name text
)
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $malicious$ INSERT INTO {}.invocation_probe VALUES (1)
RETURNING 'x'::text, 'x'::text, 'x'::text, 'x'::text, 'x'::text, 'x'::text,
          'x'::text, 'x'::text, 'x'::text, 'x'::text, 'x'::text, 'x'::text $malicious$"#,
        fixture.trusted, fixture.trusted
    );
    sqlx::query(&malicious)
        .execute(&fixture.admin)
        .await
        .expect("replace body while preserving comment and ACL");
    assert!(matches!(
        inspect_installation(
            &fixture.installer_options,
            &fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::InstallationConflict)
    ));
    let calls: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.invocation_probe",
        fixture.trusted
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("probe count");
    assert_eq!(calls, 0, "the unverified definer function must not execute");
    fixture.cleanup().await;
}

#[tokio::test]
async fn same_id_installers_with_repeatable_read_default_converge_after_lock_wait() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    sqlx::query(&format!(
        "ALTER ROLE {} IN DATABASE {} SET default_transaction_isolation = 'repeatable read'",
        fixture.installer, fixture.database_name
    ))
    .execute(&fixture.admin)
    .await
    .expect("set installer RR default");
    let mut blocker = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("blocker");
    sqlx::query("BEGIN")
        .execute(&mut blocker)
        .await
        .expect("begin blocker");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(6_841_445_497_037_937_993_i64)
        .execute(&mut blocker)
        .await
        .expect("hold lineage lock");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await
        .expect("blocker pid");
    let prefix = format!("attestor_race_{}", uuid::Uuid::new_v4().simple());
    let first_name = format!("{prefix}_a");
    let second_name = format!("{prefix}_b");
    let first_options = fixture
        .installer_options
        .clone()
        .application_name(&first_name);
    let second_options = fixture
        .installer_options
        .clone()
        .application_name(&second_name);
    let first_config = fixture.config();
    let second_config = fixture.config();
    let operation = fixture.operation_id;
    let first = tokio::spawn(async move { install(&first_options, first_config, operation).await });
    let second =
        tokio::spawn(async move { install(&second_options, second_config, operation).await });
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE application_name = ANY($1) AND $2 = ANY(pg_catalog.pg_blocking_pids(pid))")
                .bind(vec![first_name.clone(), second_name.clone()]).bind(blocker_pid)
                .fetch_one(&fixture.admin).await.expect("observe waiting installers");
            if count == 2 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("both installers must queue before release");
    sqlx::query("COMMIT")
        .execute(&mut blocker)
        .await
        .expect("release lineage lock");
    first
        .await
        .expect("join first installer")
        .expect("first same-ID install");
    second
        .await
        .expect("join second installer")
        .expect("second same-ID retry");
    assert_eq!(
        inspect_installation(&fixture.installer_options, &fixture.config(), operation)
            .await
            .expect("read installation"),
        InstallationStatus::Committed
    );
    drop(blocker);
    fixture.cleanup().await;
}

trait ProxyStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> ProxyStream for T {}

async fn lose_commit_reply_proxy(
    listener: tokio::net::TcpListener,
    target: PgConnectOptions,
    reply_held: std::sync::Arc<tokio::sync::Notify>,
    disconnect: std::sync::Arc<tokio::sync::Notify>,
) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (client, _) = listener.accept().await?;
    let server: Box<dyn ProxyStream> = if let Some(socket) = target.get_socket() {
        let path = socket.join(format!(".s.PGSQL.{}", target.get_port()));
        Box::new(tokio::net::UnixStream::connect(path).await?)
    } else {
        Box::new(tokio::net::TcpStream::connect((target.get_host(), target.get_port())).await?)
    };
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let hold = std::sync::Arc::new(AtomicBool::new(false));
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
                // Drop both socket halves without forwarding COMMIT's reply.
                return Ok(());
            }
            client_write.write_all(&buffer[..count]).await?;
        }
    };
    tokio::select! { result = upstream => result, result = downstream => result }
}

#[tokio::test]
async fn lost_commit_ack_resolves_by_fresh_exact_operation_readback() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let reply_held = std::sync::Arc::new(tokio::sync::Notify::new());
    let disconnect = std::sync::Arc::new(tokio::sync::Notify::new());
    let proxy = tokio::spawn(lose_commit_reply_proxy(
        listener,
        fixture.admin_options.clone(),
        reply_held.clone(),
        disconnect.clone(),
    ));
    let mut proxy_url = url::Url::parse(&fixture.base_url).expect("test URL");
    proxy_url.set_host(Some("127.0.0.1")).expect("proxy host");
    proxy_url.set_port(Some(port)).expect("proxy port");
    let retained: Vec<_> = proxy_url
        .query_pairs()
        .filter(|(key, _)| {
            !matches!(
                key.as_ref(),
                "host" | "hostaddr" | "port" | "sslmode" | "user" | "options"
            )
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    proxy_url
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("user", &fixture.installer)
        .append_pair("sslmode", "disable")
        .append_pair("options", &format!("-c search_path={}", fixture.target));
    let options = PgConnectOptions::from_str(proxy_url.as_str()).expect("proxy installer options");
    let config = fixture.config();
    let operation = fixture.operation_id;
    let pending = tokio::spawn(async move { install(&options, config, operation).await });
    tokio::time::timeout(Duration::from_secs(5), reply_held.notified())
        .await
        .expect("hold COMMIT acknowledgement");
    let observed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.proname = 'read_physical_identity' AND pg_catalog.obj_description(p.oid, 'pg_proc') LIKE $2)")
        .bind(&fixture.trusted).bind(format!("waddle-attestation-v1:{}:%", operation))
        .fetch_one(&fixture.admin).await.expect("observe committed operation marker");
    assert!(
        observed,
        "server must commit before acknowledgement is lost"
    );
    disconnect.notify_one();
    proxy.await.expect("join proxy").expect("proxy I/O");
    assert!(matches!(pending.await.expect("join installer"),
        Err(PhysicalAttestationError::CommitUnknown { operation_id }) if operation_id == operation));
    assert_eq!(
        inspect_installation(&fixture.installer_options, &fixture.config(), operation)
            .await
            .expect("fresh exact readback"),
        InstallationStatus::Committed
    );
    install(&fixture.installer_options, fixture.config(), operation)
        .await
        .expect("same-ID retry resolves final state");
    fixture.cleanup().await;
}

#[tokio::test]
async fn cancelled_lock_wait_is_indeterminate_and_same_id_retry_converges() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let mut blocker = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("blocker");
    sqlx::query("BEGIN")
        .execute(&mut blocker)
        .await
        .expect("begin blocker");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(6_841_445_497_037_937_993_i64)
        .execute(&mut blocker)
        .await
        .expect("hold lineage lock");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await
        .expect("blocker pid");
    let application = format!("attestor_cancel_{}", uuid::Uuid::new_v4().simple());
    let options = fixture
        .installer_options
        .clone()
        .application_name(&application);
    let config = fixture.config();
    let operation = fixture.operation_id;
    let pending = tokio::spawn(async move { install(&options, config, operation).await });
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE application_name=$1 AND $2 = ANY(pg_catalog.pg_blocking_pids(pid)))")
                .bind(&application).bind(blocker_pid).fetch_one(&fixture.admin).await.expect("observe cancelled waiter");
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("installer must wait on the identified lock");
    pending.abort();
    assert!(pending
        .await
        .expect_err("installer task cancelled")
        .is_cancelled());
    sqlx::query("COMMIT")
        .execute(&mut blocker)
        .await
        .expect("release blocker");
    assert_eq!(
        inspect_installation(&fixture.installer_options, &fixture.config(), operation)
            .await
            .expect("inspect after cancellation"),
        InstallationStatus::Indeterminate
    );
    install(&fixture.installer_options, fixture.config(), operation)
        .await
        .expect("same-ID retry");
    assert_eq!(
        inspect_installation(&fixture.installer_options, &fixture.config(), operation)
            .await
            .expect("inspect retry"),
        InstallationStatus::Committed
    );
    drop(blocker);
    fixture.cleanup().await;
}

#[tokio::test]
async fn installer_direct_privilege_drift_aborts_before_install() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let checks = [
        (
            format!(
                "GRANT UPDATE(lineage_uuid) ON {}._lineage TO {}",
                fixture.target, fixture.installer
            ),
            format!(
                "REVOKE UPDATE(lineage_uuid) ON {}._lineage FROM {}",
                fixture.target, fixture.installer
            ),
            "installer lineage modification privileges",
        ),
        (
            format!(
                "GRANT DELETE ON {}._lineage TO {}",
                fixture.target, fixture.installer
            ),
            format!(
                "REVOKE DELETE ON {}._lineage FROM {}",
                fixture.target, fixture.installer
            ),
            "installer lineage modification privileges",
        ),
        (
            format!(
                "GRANT CREATE ON SCHEMA {} TO {}",
                fixture.target, fixture.installer
            ),
            format!(
                "REVOKE CREATE ON SCHEMA {} FROM {}",
                fixture.target, fixture.installer
            ),
            "installer, runtime or definer CREATE/TEMP",
        ),
        (
            format!(
                "GRANT CREATE ON DATABASE {} TO {}",
                fixture.database_name, fixture.installer
            ),
            format!(
                "REVOKE CREATE ON DATABASE {} FROM {}",
                fixture.database_name, fixture.installer
            ),
            "installer, runtime or definer CREATE/TEMP",
        ),
        (
            format!(
                "GRANT TEMPORARY ON DATABASE {} TO {}",
                fixture.database_name, fixture.installer
            ),
            format!(
                "REVOKE TEMPORARY ON DATABASE {} FROM {}",
                fixture.database_name, fixture.installer
            ),
            "installer, runtime or definer CREATE/TEMP",
        ),
        (
            format!(
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {}",
                fixture.installer
            ),
            format!(
                "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {}",
                fixture.installer
            ),
            "installer control function privileges",
        ),
    ];
    for (grant, revoke, expected) in checks {
        sqlx::query(&grant)
            .execute(&fixture.admin)
            .await
            .expect("grant unexpected installer privilege");
        match install(
            &fixture.installer_options,
            fixture.config(),
            fixture.operation_id,
        )
        .await
        {
            Err(PhysicalAttestationError::Mismatch { field }) => assert_eq!(field, expected),
            result => panic!("unexpected installer privilege was not rejected: {result:?}"),
        }
        sqlx::query(&revoke)
            .execute(&fixture.admin)
            .await
            .expect("revoke unexpected installer privilege");
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname=$1 AND p.proname='read_physical_identity'")
        .bind(&fixture.trusted).fetch_one(&fixture.admin).await.expect("function count");
    assert_eq!(
        count, 0,
        "privilege drift must fail before function creation"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn installer_lock_timeout_reports_indeterminate_without_mutation() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let mut blocker = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("blocker");
    sqlx::query("BEGIN")
        .execute(&mut blocker)
        .await
        .expect("begin blocker");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(6_841_445_497_037_937_993_i64)
        .execute(&mut blocker)
        .await
        .expect("hold lineage lock");
    assert!(matches!(
        install(
            &fixture.installer_options,
            fixture.config(),
            fixture.operation_id
        )
        .await,
        Err(PhysicalAttestationError::InstallationIndeterminate)
    ));
    sqlx::query("COMMIT")
        .execute(&mut blocker)
        .await
        .expect("release blocker");
    assert_eq!(
        inspect_installation(
            &fixture.installer_options,
            &fixture.config(),
            fixture.operation_id
        )
        .await
        .expect("inspect after timeout"),
        InstallationStatus::Indeterminate
    );
    drop(blocker);
    fixture.cleanup().await;
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn full_identity_writer_preserves_v1009_candidate_and_temp_shadow_contracts() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (_boundary_url, config) = prepare_attested_writer_fixture(&fixture).await;
    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config,
        1,
        Duration::from_secs(2),
    )
    .expect("attested runtime pool");
    let mut connection = pool.acquire().await.expect("attested runtime connection");
    let system_identifier_privilege: bool = sqlx::query_scalar(
        "SELECT pg_catalog.has_function_privilege(current_user, 'pg_catalog.pg_control_system()', 'EXECUTE')",
    )
    .fetch_one(&mut *connection)
    .await
    .expect("inspect runtime system-identifier privilege");
    assert!(
        !system_identifier_privilege,
        "runtime attestation must use the trusted definer, not pg_control_system privilege"
    );
    let current_schema: String = sqlx::query_scalar("SELECT pg_catalog.current_schema()")
        .fetch_one(&mut *connection)
        .await
        .expect("active runtime schema");
    assert_eq!(current_schema, fixture.target);
    sqlx::query(
        "CREATE TEMP TABLE ingress_protocol_epoch (id INTEGER PRIMARY KEY, epoch BIGINT NOT NULL)",
    )
    .execute(&mut *connection)
    .await
    .expect("create temporary epoch shadow");
    sqlx::query("INSERT INTO ingress_protocol_epoch (id, epoch) VALUES (1, 1)")
        .execute(&mut *connection)
        .await
        .expect("seed hostile temporary epoch shadow");
    let shadow_schema: String = sqlx::query_scalar(
        "SELECT n.nspname FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.oid = pg_catalog.to_regclass('ingress_protocol_epoch')",
    )
    .fetch_one(&mut *connection)
    .await
    .expect("resolve temporary relation shadow");
    assert!(shadow_schema.starts_with("pg_temp_"));
    drop(connection);

    let database = waddle_server::test_support::database_from_attested_postgres_pool(
        "full-identity-writer",
        pool,
    );
    let writer_admin = PgPool::connect_with(fixture.admin_options.clone())
        .await
        .expect("schema-qualified writer observer");

    // The dark UoW still holds V1009's row-share lock until commit. An epoch
    // transition cannot pass the writer while it owns that lock.
    let lineage = LineageConfig {
        deployment_uuid: Some(fixture.lineage.deployment_uuid),
        action: None,
    };
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let held_database = database.clone();
    let held_lineage = lineage.clone();
    let held_writer = tokio::spawn(async move {
        waddle_server::test_support::hold_attested_uow_epoch_lock(
            held_database,
            held_lineage,
            ready_tx,
            release_rx,
        )
        .await
    });
    let readiness = tokio::time::timeout(Duration::from_secs(5), ready_rx)
        .await
        .expect("attested writer acquired epoch");
    let writer_pid = match readiness {
        Ok(pid) => pid,
        Err(_) => {
            let result = held_writer.await;
            panic!("attested UoW failed before readiness: {result:?}");
        }
    };
    let epoch_relation = format!("\"{}\".ingress_protocol_epoch", fixture.target);
    let row_share_held: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks \
         WHERE pid = $1 AND relation = pg_catalog.to_regclass($2) \
           AND mode = 'RowShareLock' AND granted)",
    )
    .bind(writer_pid)
    .bind(&epoch_relation)
    .fetch_one(&writer_admin)
    .await
    .expect("inspect V1009 row-share lock");
    assert!(row_share_held, "dark writer retains V1009's epoch row lock");

    let mut transition = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("epoch transition connection");
    sqlx::query("BEGIN")
        .execute(&mut transition)
        .await
        .expect("begin epoch transition");
    let transition_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut transition)
        .await
        .expect("transition backend PID");
    let transition_target = fixture.target.clone();
    let transition_task = tokio::spawn(async move {
        sqlx::query(&format!(
            "UPDATE \"{transition_target}\".ingress_protocol_epoch \
             SET epoch = 1, activated_at = now(), lineage_uuid = $1::uuid WHERE id = 1"
        ))
        .bind(uuid::Uuid::new_v4())
        .execute(&mut transition)
        .await
        .expect("transition waits for the attested writer");
        sqlx::query("ROLLBACK")
            .execute(&mut transition)
            .await
            .expect("roll back epoch transition");
    });
    wait_for_blocking_pid(&writer_admin, transition_pid, writer_pid).await;
    release_tx.send(()).expect("release epoch-lock writer");
    held_writer
        .await
        .expect("join held writer")
        .expect("commit held attested UoW");
    transition_task.await.expect("join transition waiter");
    let live_epoch: i64 = sqlx::query_scalar(&format!(
        "SELECT epoch FROM \"{}\".ingress_protocol_epoch WHERE id = 1",
        fixture.target
    ))
    .fetch_one(&writer_admin)
    .await
    .expect("read rolled-back transition");
    assert_eq!(live_epoch, 0);

    sqlx::raw_sql(include_str!(
        "fixtures/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&writer_admin)
    .await
    .expect("install unregistered candidate relation guard");
    let count_before = ingress_message_count(&writer_admin, &fixture.target).await;
    assert!(
        waddle_server::test_support::legacy_ingress_candidate_write(
            &database,
            uuid::Uuid::new_v4()
        )
        .await
        .is_err(),
        "legacy writer without the schema prelock is rejected"
    );
    assert_eq!(
        ingress_message_count(&writer_admin, &fixture.target).await,
        count_before,
        "legacy writer failure happens before application DML"
    );

    waddle_server::test_support::attested_raw_ingress_candidate_write(
        &database,
        &database,
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("full-identity raw writer passes V1009 and candidate guard");
    waddle_server::test_support::attested_uow_candidate_write(
        &database,
        lineage,
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("full-identity UoW passes V1009 and candidate guard");
    waddle_server::test_support::attested_substrate_candidate_write(
        &database,
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("full-identity substrate writer passes candidate guard");
    assert_eq!(
        ingress_message_count(&writer_admin, &fixture.target).await,
        count_before + 3,
        "TEMP shadow cannot redirect schema-qualified epoch guarding"
    );

    assert!(
        waddle_server::test_support::attested_raw_candidate_write_with_epoch(
            &database,
            &database,
            uuid::Uuid::new_v4(),
            "1",
        )
        .await
        .is_err(),
        "candidate rejects a stale transaction-local epoch proof"
    );
    assert_eq!(
        ingress_message_count(&writer_admin, &fixture.target).await,
        count_before + 3,
        "wrong proof is rejected before application DML"
    );

    drop(database);
    writer_admin.close().await;
    fixture.cleanup().await;
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn full_identity_writer_fails_closed_for_database_schema_pool_and_attestor_drift() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url.clone()).await;
    let (boundary_url, config) = prepare_attested_writer_fixture(&fixture).await;
    let writer_admin = PgPool::connect_with(fixture.admin_options.clone())
        .await
        .expect("schema-qualified writer observer");
    sqlx::raw_sql(include_str!(
        "fixtures/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&writer_admin)
    .await
    .expect("install candidate relation guard");
    let count_before = ingress_message_count(&writer_admin, &fixture.target).await;

    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        1,
        Duration::from_secs(2),
    )
    .expect("attested runtime pool");
    let database = waddle_server::test_support::database_from_attested_postgres_pool(
        "full-identity-writer-fail-closed",
        pool,
    );

    let ordinary_pool = PgPool::connect_with(fixture.runtime_options.clone())
        .await
        .expect("ordinary runtime pool to the same database");
    let ordinary_database = waddle_server::test_support::database_from_unattested_postgres_pool(
        "same-database-wrong-pool",
        ordinary_pool,
    );
    assert!(
        waddle_server::test_support::attested_raw_ingress_candidate_write(
            &ordinary_database,
            &database,
            uuid::Uuid::new_v4(),
        )
        .await
        .is_err(),
        "capability from another pool cannot authorize a writer"
    );

    let mut wrong_schema_lineage = fixture.lineage.clone();
    wrong_schema_lineage
        .postgres_identity
        .as_mut()
        .expect("PostgreSQL lineage")
        .schema
        .oid ^= 1;
    let wrong_schema_config = fixture.with_lineage(wrong_schema_lineage);
    let wrong_schema_pool = attested_pool(
        fixture.runtime_options.clone(),
        wrong_schema_config,
        1,
        Duration::from_secs(2),
    )
    .expect("schema-mismatched attested pool constructor");
    let wrong_schema_database = waddle_server::test_support::database_from_attested_postgres_pool(
        "wrong-schema-identity",
        wrong_schema_pool,
    );
    assert!(
        waddle_server::test_support::attested_raw_ingress_candidate_write(
            &wrong_schema_database,
            &wrong_schema_database,
            uuid::Uuid::new_v4(),
        )
        .await
        .is_err(),
        "schema OID drift fails physical attestation before DML"
    );

    let hostile_schema = format!("waddle_attest_hostile_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {hostile_schema}"))
        .execute(&writer_admin)
        .await
        .expect("create hostile search-path schema");
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {hostile_schema} TO {}",
        fixture.runtime
    ))
    .execute(&writer_admin)
    .await
    .expect("runtime can see hostile search-path schema");
    let hostile_url = url_with_search_path(
        &boundary_url,
        &format!("{hostile_schema},{}", fixture.target),
    );
    let hostile_options = PgConnectOptions::from_str(&hostile_url)
        .expect("hostile search-path options")
        .username(&fixture.runtime);
    let hostile_pool = attested_pool(hostile_options, config.clone(), 1, Duration::from_secs(2))
        .expect("hostile-path pool is refused only when physically checked");
    let hostile_database = waddle_server::test_support::database_from_attested_postgres_pool(
        "hostile-search-path",
        hostile_pool,
    );
    assert!(
        waddle_server::test_support::attested_raw_ingress_candidate_write(
            &hostile_database,
            &hostile_database,
            uuid::Uuid::new_v4(),
        )
        .await
        .is_err(),
        "poisoned current_schema fails closed before DML"
    );

    // Provision a second, fully valid identity so the attestation reaches the
    // trusted function and rejects the connected database by its live name,
    // rather than failing early because its schema or function is absent.
    let wrong_database_fixture = Fixture::new(url.clone()).await;
    let _ = prepare_attested_writer_fixture(&wrong_database_fixture).await;
    sqlx::raw_sql(include_str!(
        "fixtures/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&wrong_database_fixture.admin)
    .await
    .expect("install candidate guard in wrong-database fixture");
    let wrong_database_count = ingress_message_count(
        &wrong_database_fixture.admin,
        &wrong_database_fixture.target,
    )
    .await;
    let mut expected_original_database = wrong_database_fixture.lineage.clone();
    expected_original_database
        .postgres_identity
        .as_mut()
        .expect("wrong fixture PostgreSQL identity")
        .database
        .name = PgDatabaseName(fixture.database_name.clone());
    let wrong_database_identity_config =
        wrong_database_fixture.with_lineage(expected_original_database);
    let wrong_database_options = wrong_database_fixture.runtime_options.clone();
    let wrong_database_pool = attested_pool(
        wrong_database_options,
        wrong_database_identity_config,
        1,
        Duration::from_secs(2),
    )
    .expect("wrong-database pool constructor");
    let wrong_database = waddle_server::test_support::database_from_attested_postgres_pool(
        "wrong-database-identity",
        wrong_database_pool,
    );
    assert!(
        waddle_server::test_support::attested_raw_ingress_candidate_write(
            &wrong_database,
            &wrong_database,
            uuid::Uuid::new_v4(),
        )
        .await
        .is_err(),
        "wrong current database fails before application DML"
    );
    drop(wrong_database);
    assert_eq!(
        ingress_message_count(
            &wrong_database_fixture.admin,
            &wrong_database_fixture.target
        )
        .await,
        wrong_database_count,
        "wrong-database identity mismatch precedes application DML"
    );
    wrong_database_fixture.cleanup().await;

    sqlx::query(&format!(
        "UPDATE \"{}\"._lineage SET format = 2 WHERE id = 1",
        fixture.target
    ))
    .execute(&writer_admin)
    .await
    .expect("poison lineage format");
    assert!(
        waddle_server::test_support::attested_raw_ingress_candidate_write(
            &database,
            &database,
            uuid::Uuid::new_v4(),
        )
        .await
        .is_err(),
        "unsupported lineage format fails before application DML"
    );
    sqlx::query(&format!(
        "UPDATE \"{}\"._lineage SET format = 1 WHERE id = 1",
        fixture.target
    ))
    .execute(&writer_admin)
    .await
    .expect("restore supported lineage format");

    let wrong_deployment = LineageConfig {
        deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
        action: None,
    };
    assert!(
        waddle_server::test_support::attested_uow_candidate_write(
            &database,
            wrong_deployment,
            uuid::Uuid::new_v4(),
        )
        .await
        .is_err(),
        "attested UoW refuses a caller deployment mismatch before DML"
    );
    assert_eq!(
        ingress_message_count(&writer_admin, &fixture.target).await,
        count_before,
        "deployment mismatch does not reach application DML"
    );

    sqlx::query(&format!(
        "GRANT {} TO {}",
        fixture.outsider, fixture.runtime
    ))
    .execute(&fixture.cluster_admin)
    .await
    .expect("allow deliberate SET ROLE poisoning");
    let role_pool = attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        1,
        Duration::from_secs(2),
    )
    .expect("role-poisoning pool");
    let mut role_connection = role_pool.acquire().await.expect("runtime session");
    sqlx::query(&format!("SET ROLE {}", fixture.outsider))
        .execute(&mut *role_connection)
        .await
        .expect("poison effective role");
    assert!(
        attest_connection(&mut role_connection, &config)
            .await
            .is_err(),
        "physical attestor refuses the role-poisoned connection"
    );
    drop(role_connection);
    let role_database = waddle_server::test_support::database_from_attested_postgres_pool(
        "wrong-effective-role",
        role_pool,
    );
    waddle_server::test_support::attested_raw_ingress_candidate_write(
        &role_database,
        &role_database,
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("pool discards the role-poisoned session before its verified replacement writes");
    sqlx::query(&format!(
        "REVOKE {} FROM {}",
        fixture.outsider, fixture.runtime
    ))
    .execute(&fixture.cluster_admin)
    .await
    .expect("remove deliberate SET ROLE membership");

    sqlx::query(&format!(
        "DROP FUNCTION {}.read_physical_identity()",
        fixture.trusted
    ))
    .execute(&writer_admin)
    .await
    .expect("remove physical attestor function");
    assert!(
        waddle_server::test_support::attested_raw_ingress_candidate_write(
            &database,
            &database,
            uuid::Uuid::new_v4(),
        )
        .await
        .is_err(),
        "missing trusted function fails before application DML"
    );

    assert_eq!(
        ingress_message_count(&writer_admin, &fixture.target).await,
        count_before + 1,
        "only the writer on the replacement session that passed attestation reaches application DML"
    );
    assert!(matches!(
        attested_pool(
            fixture.admin_options.clone(),
            config,
            1,
            Duration::from_secs(2)
        ),
        Err(PhysicalAttestationError::WrongConfiguredLogin)
    ));

    drop(database);
    drop(ordinary_database);
    drop(wrong_schema_database);
    drop(hostile_database);
    drop(role_database);
    writer_admin.close().await;
    fixture.cleanup().await;
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn full_identity_receipt_writer_targets_real_tables_and_reconciles_commit_outcomes() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, key) = typed_authority_fixture(&fixture).await;
    for statement in [
        format!(
            "GRANT INSERT ON {}.ingress_messages TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT UPDATE ON {}.ingress_protocol_epoch TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT SELECT ON {}.ingress_effect_intents TO {}",
            fixture.target, fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("grant dark UoW writer fixture privileges");
    }
    sqlx::query(&format!(
        "GRANT TEMPORARY ON DATABASE {} TO {}",
        fixture.database_name, fixture.runtime
    ))
    .execute(&fixture.cluster_admin)
    .await
    .expect("grant temp only for persistent shadow fixture");

    let pool = attested_pool(
        fixture.runtime_options.clone(),
        config,
        1,
        Duration::from_secs(3),
    )
    .expect("one-slot attested receipt pool");
    let observer_pool = pool.clone();
    let mut connection = pool.acquire().await.expect("attested runtime checkout");
    sqlx::query(
        "CREATE TEMP TABLE ingress_effect_receipts \
         (message_key UUID, kind INTEGER, semantic_identity_hash BYTEA)",
    )
    .execute(&mut *connection)
    .await
    .expect("create persistent temporary receipt shadow");
    sqlx::query(
        "CREATE TEMP TABLE ingress_messages \
         (message_key UUID, digest_version INTEGER, digest BYTEA)",
    )
    .execute(&mut *connection)
    .await
    .expect("create persistent temporary message shadow");
    drop(connection);

    let database = waddle_server::test_support::database_from_attested_postgres_pool(
        "attested-receipt-writer",
        pool,
    );
    let writer_admin = PgPool::connect_with(fixture.admin_options.clone())
        .await
        .expect("schema-qualified receipt observer");

    // Existing unqualified message SQL must resolve to the target schema in
    // the attested transaction, despite a persistent TEMP relation shadow.
    let count_before = ingress_message_count(&writer_admin, &fixture.target).await;
    waddle_server::test_support::attested_uow_candidate_write(
        &database,
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("prepared UoW message write");
    assert_eq!(
        ingress_message_count(&writer_admin, &fixture.target).await,
        count_before + 1,
        "message write lands in the attested schema"
    );

    // A lost successful pooled COMMIT acknowledgement is resolved only by a
    // fresh attested read of the exact receipt key. Pool size one proves the
    // write transaction is committed before that readback checks out again.
    waddle_server::test_support::attested_pooled_receipt_write_after_lost_ack(
        &database,
        key,
        EffectReceiptKind::from_storage(1),
        &[1_u8; 32],
    )
    .await
    .expect("exact-key readback confirms committed pooled receipt");

    let (present, complete, key_count) = waddle_server::test_support::attested_uow_receipt_write(
        &database,
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
        key,
        EffectReceiptKind::from_storage(1),
        &[1_u8; 32],
    )
    .await
    .expect("caller-owned UoW receipt write and read");
    assert!(present);
    assert!(complete);
    assert_eq!(key_count, 1);

    let target_receipt_count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM \"{}\".ingress_effect_receipts WHERE message_key = $1",
        fixture.target
    ))
    .bind(key.to_storage())
    .fetch_one(&writer_admin)
    .await
    .expect("read target receipt");
    assert_eq!(target_receipt_count, 1, "same key is idempotent");
    let mut shadow_observer = observer_pool
        .acquire()
        .await
        .expect("reacquire one-slot attested observer");
    let (shadow_receipts, shadow_messages): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM pg_temp.ingress_effect_receipts), \
                (SELECT count(*) FROM pg_temp.ingress_messages)",
    )
    .fetch_one(&mut *shadow_observer)
    .await
    .expect("inspect persistent temporary shadows");
    assert_eq!(shadow_receipts, 0, "receipt SQL bypasses TEMP shadow");
    assert_eq!(shadow_messages, 0, "message SQL bypasses TEMP shadow");
    drop(shadow_observer);

    let failed_key = MessageKey::from_storage(uuid::Uuid::new_v4());
    let failed_hash = [9_u8; 32];
    let mut seed = fixture
        .admin
        .begin()
        .await
        .expect("seed failed-commit intent");
    sqlx::query(&format!(
        "LOCK TABLE ONLY \"{}\".ingress_protocol_epoch IN ACCESS SHARE MODE",
        fixture.target
    ))
    .execute(&mut *seed)
    .await
    .expect("seed epoch relation prelock");
    sqlx::query(
        "SELECT set_config('waddle.protocol_epoch', '0', true), \
                 set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)",
    )
    .execute(&mut *seed)
    .await
    .expect("seed V1009 proof");
    sqlx::query(&format!(
        "INSERT INTO \"{}\".ingress_messages \
         (message_key, digest_version, digest) VALUES ($1, 1, $2)",
        fixture.target
    ))
    .bind(failed_key.to_storage())
    .bind([7_u8; 32].as_slice())
    .execute(&mut *seed)
    .await
    .expect("seed failed-commit message");
    sqlx::query(&format!(
        "INSERT INTO \"{}\".ingress_effect_intents \
         (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) \
         VALUES ($1, 1, 2, $2, 1, $3)",
        fixture.target
    ))
    .bind(failed_key.to_storage())
    .bind(failed_hash.as_slice())
    .bind(Vec::<u8>::new())
    .execute(&mut *seed)
    .await
    .expect("seed failed-commit intent");
    seed.commit()
        .await
        .expect("commit failed-commit fixture rows");

    let function = format!("\"{}\".fail_receipt_commit", fixture.target);
    let trigger = "fail_receipt_commit";
    sqlx::query(&format!(
        "CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'forced deferred receipt commit failure'; END $$"
    ))
    .execute(&writer_admin)
    .await
    .expect("create deferred commit failure function");
    sqlx::query(&format!(
        "CREATE CONSTRAINT TRIGGER {trigger} AFTER INSERT ON \"{}\".ingress_effect_receipts \
         DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {function}()",
        fixture.target
    ))
    .execute(&writer_admin)
    .await
    .expect("create deferred receipt commit failure");
    let failed = waddle_server::ingress_uow::EffectReceiptRepository::record_receipt_pooled(
        &database,
        failed_key,
        EffectReceiptKind::from_storage(2),
        &failed_hash,
    )
    .await;
    assert!(
        matches!(
            &failed,
            Err(waddle_server::ingress_uow::IngressUowError::Substrate(
                IngressSubstrateError::ReceiptCommitIndeterminate
            ))
        ),
        "expected absent failed receipt to remain indeterminate, got {failed:?}"
    );
    let failed_count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM \"{}\".ingress_effect_receipts WHERE message_key = $1",
        fixture.target
    ))
    .bind(failed_key.to_storage())
    .fetch_one(&writer_admin)
    .await
    .expect("confirm failed transaction has no committed receipt");
    assert_eq!(failed_count, 0, "absence remains indeterminate");
    sqlx::query(&format!(
        "DROP TRIGGER {trigger} ON \"{}\".ingress_effect_receipts",
        fixture.target
    ))
    .execute(&writer_admin)
    .await
    .expect("drop deferred receipt trigger");
    sqlx::query(&format!("DROP FUNCTION {function}()"))
        .execute(&writer_admin)
        .await
        .expect("drop deferred receipt function");

    drop(database);
    drop(observer_pool);
    writer_admin.close().await;
    fixture.cleanup().await;
}
