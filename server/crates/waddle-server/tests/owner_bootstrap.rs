//! Real PostgreSQL checks for the dark P2.1 owner/runtime split.

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use waddle_server::config::LineageConfig;
use waddle_server::db::bootstrap::{
    bootstrap_global_owner, bootstrap_store_owner, verify_global_runtime,
    verify_store_lineage_runtime, RuntimeAttachmentError,
};
use waddle_server::db::lineage::{
    self, AttestedLineage, DeploymentUuid, LineageAction, LineageUuid,
};
use waddle_server::db::{
    Database, DatabaseConfig, DatabaseDriver, DatabaseError, MigrationLedgerError,
};
use waddle_server::inbox::{
    bootstrap_inbox_owner_schema, DatabaseInboxStorage, InboxRuntimeAttachError,
};
use waddle_server::pending_delivery::bootstrap_pending_delivery_owner_schema;
use waddle_server::sm_persistence::bootstrap_portable_sm_owner_schema;

static CATALOG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    admin: PgPool,
    owner: Database,
    runtime: Database,
    owner_options: PgConnectOptions,
    runtime_options: PgConnectOptions,
    schema: String,
    owner_role: String,
    runtime_role: String,
    deployment_uuid: DeploymentUuid,
    base_url: String,
}

impl Fixture {
    async fn new(url: String) -> Self {
        let admin = PgPool::connect(&url).await.expect("admin PostgreSQL pool");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let schema = format!("waddle_boot_s_{suffix}");
        let owner_role = format!("waddle_boot_o_{suffix}");
        let runtime_role = format!("waddle_boot_r_{suffix}");
        for role in [&owner_role, &runtime_role] {
            sqlx::query(&format!(
                "CREATE ROLE {role} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"
            ))
            .execute(&admin)
            .await
            .expect("distinct restricted login");
            sqlx::query(&format!(
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {role}"
            ))
            .execute(&admin)
            .await
            .expect("temporary direct identity-read grant for dark test");
        }
        sqlx::query(&format!(
            "CREATE SCHEMA {schema} AUTHORIZATION {owner_role}"
        ))
        .execute(&admin)
        .await
        .expect("owner schema");
        let owner_url = url_with_schema_user(&url, &schema, &owner_role);
        let runtime_url = url_with_schema_user(&url, &schema, &runtime_role);
        let owner = open_database("owner", &owner_url).await;
        let runtime = open_database("runtime", &runtime_url).await;
        Self {
            admin,
            owner,
            runtime,
            owner_options: PgConnectOptions::from_str(&owner_url).expect("owner options"),
            runtime_options: PgConnectOptions::from_str(&runtime_url).expect("runtime options"),
            schema,
            owner_role,
            runtime_role,
            deployment_uuid: DeploymentUuid(uuid::Uuid::new_v4()),
            base_url: url,
        }
    }

    fn owner_config(&self) -> LineageConfig {
        LineageConfig {
            deployment_uuid: Some(self.deployment_uuid),
            action: Some(LineageAction::Enroll),
        }
    }

    fn runtime_config(&self) -> LineageConfig {
        LineageConfig {
            deployment_uuid: Some(self.deployment_uuid),
            action: None,
        }
    }

    async fn bootstrap(&self) {
        bootstrap_global_owner(&self.owner, &self.owner_config(), true)
            .await
            .expect("owner bootstrap");
        for table in ["_lineage", "_migrations"] {
            sqlx::query(&format!(
                "GRANT SELECT ON TABLE {}.{table} TO {}",
                self.schema, self.runtime_role
            ))
            .execute(&self.admin)
            .await
            .expect("runtime read grant");
        }
        sqlx::query(&format!(
            "GRANT USAGE ON SCHEMA {} TO {}",
            self.schema, self.runtime_role
        ))
        .execute(&self.admin)
        .await
        .expect("runtime schema usage");
    }

    async fn bootstrap_store(&self) {
        bootstrap_store_owner(&self.owner, &self.owner_config())
            .await
            .expect("store owner bootstrap");
        sqlx::query(&format!(
            "GRANT SELECT ON TABLE {}._lineage TO {}",
            self.schema, self.runtime_role
        ))
        .execute(&self.admin)
        .await
        .expect("runtime lineage read grant");
        sqlx::query(&format!(
            "GRANT USAGE ON SCHEMA {} TO {}",
            self.schema, self.runtime_role
        ))
        .execute(&self.admin)
        .await
        .expect("runtime schema usage");
    }

    async fn expected_identity(&self) -> AttestedLineage {
        lineage::verify(&self.owner, &self.runtime_config())
            .await
            .expect("owner-produced committed identity")
    }

    async fn cleanup(self) {
        drop(self.owner);
        drop(self.runtime);
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop owned schema");
        for role in [&self.owner_role, &self.runtime_role] {
            sqlx::query(&format!(
                "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {role}"
            ))
            .execute(&self.admin)
            .await
            .expect("revoke identity-read grant");
            sqlx::query(&format!("DROP ROLE {role}"))
                .execute(&self.admin)
                .await
                .expect("drop test role");
        }
        self.admin.close().await;
    }
}

async fn open_database(name: &str, url: &str) -> Database {
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url);
    config.pool_size = 1;
    Database::from_config(name, &config)
        .await
        .expect("test database")
}

fn url_with_schema_user(base: &str, schema: &str, user: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "options" && key != "user")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("user", user)
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

fn test_url() -> Option<String> {
    waddle_ws_test_support::require_test_postgres_url()
}

#[tokio::test]
async fn owner_bootstrap_commits_before_restricted_runtime_attachment() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    fixture.bootstrap().await;

    let mut owner = PgConnection::connect_with(&fixture.owner_options)
        .await
        .expect("owner login");
    let owner_session: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(&mut owner)
        .await
        .expect("owner session user");
    assert_eq!(owner_session, fixture.owner_role);
    sqlx::query(&format!(
        "CREATE FUNCTION {}.current_database() RETURNS name \
         LANGUAGE sql IMMUTABLE AS $waddle_spoof$ SELECT 'spoof_db'::name $waddle_spoof$",
        fixture.schema
    ))
    .execute(&mut owner)
    .await
    .expect("owner creates application-schema built-in shadow");
    sqlx::query(&format!(
        "SET search_path = {}, pg_catalog, pg_temp",
        fixture.schema
    ))
    .execute(&mut owner)
    .await
    .expect("set deliberately unsafe search path for control");
    let spoofed: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut owner)
        .await
        .expect("read shadowed built-in in control");
    assert_eq!(spoofed, "spoof_db", "control must prove function shadowing");
    drop(owner);
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    let runtime_session: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(&mut runtime)
        .await
        .expect("runtime session user");
    assert_eq!(runtime_session, fixture.runtime_role);
    drop(runtime);

    let before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1",
    )
    .bind(&fixture.schema)
    .fetch_one(&fixture.admin)
    .await
    .expect("catalog before attachment");
    let expected = fixture.expected_identity().await;
    let attested = verify_global_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
        .await
        .expect("restricted read-only attachment");
    assert_eq!(attested.deployment_uuid, fixture.deployment_uuid);
    assert!(attested.postgres_identity.is_some());
    let guard = fixture.runtime.guard().await.expect("runtime pool member");
    guard
        .execute("CREATE TEMP TABLE _lineage (id INTEGER)", ())
        .await
        .expect("create untrusted TEMP lineage shadow");
    guard
        .execute("CREATE TEMP TABLE _migrations (version INTEGER)", ())
        .await
        .expect("create untrusted TEMP ledger shadow");
    drop(guard);
    let after_shadow =
        verify_global_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
            .await
            .expect("trusted tables and pg_catalog functions survive shadow attempts");
    assert_eq!(after_shadow, expected);
    let after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1",
    )
    .bind(&fixture.schema)
    .fetch_one(&fixture.admin)
    .await
    .expect("catalog after attachment");
    assert_eq!(before, after, "read-only attachment created no relations");
    fixture.cleanup().await;
}

#[tokio::test]
async fn runtime_attachment_rejects_wrong_identity_schema_and_pending_ledger() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    fixture.bootstrap().await;
    let expected = fixture.expected_identity().await;

    let wrong = LineageConfig {
        deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
        action: None,
    };
    assert!(verify_global_runtime(&fixture.runtime, &wrong, &expected)
        .await
        .is_err());
    let with_action = fixture.owner_config();
    assert!(matches!(
        verify_global_runtime(&fixture.runtime, &with_action, &expected).await,
        Err(RuntimeAttachmentError::OperatorActionConfigured)
    ));
    let mut wrong_expected = expected.clone();
    wrong_expected.lineage_uuid = LineageUuid::new();
    assert!(matches!(
        verify_global_runtime(&fixture.runtime, &fixture.runtime_config(), &wrong_expected).await,
        Err(RuntimeAttachmentError::ExpectedIdentityMismatch)
    ));
    let mut incomplete_expected = expected.clone();
    incomplete_expected.postgres_identity = None;
    assert!(matches!(
        verify_global_runtime(
            &fixture.runtime,
            &fixture.runtime_config(),
            &incomplete_expected
        )
        .await,
        Err(RuntimeAttachmentError::MissingExpectedPostgresIdentity)
    ));

    let other_schema = format!("waddle_boot_x_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(
        "CREATE SCHEMA {other_schema} AUTHORIZATION {}",
        fixture.owner_role
    ))
    .execute(&fixture.admin)
    .await
    .expect("wrong schema");
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {other_schema} TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.admin)
    .await
    .expect("wrong schema usage");
    let wrong_url = url_with_schema_user(&fixture.base_url, &other_schema, &fixture.runtime_role);
    let wrong_db = open_database("wrong-schema", &wrong_url).await;
    assert!(
        verify_global_runtime(&wrong_db, &fixture.runtime_config(), &expected)
            .await
            .is_err()
    );
    drop(wrong_db);
    let wrong_owner_url =
        url_with_schema_user(&fixture.base_url, &other_schema, &fixture.owner_role);
    let wrong_owner_db = open_database("owner-wrong-schema", &wrong_owner_url).await;
    assert!(
        verify_global_runtime(&wrong_owner_db, &fixture.runtime_config(), &expected)
            .await
            .is_err()
    );
    drop(wrong_owner_db);
    let lineage_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = '_lineage'",
    )
    .bind(&other_schema)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect wrong schema after owner verifier");
    assert_eq!(
        lineage_tables, 0,
        "runtime verifier never bootstraps even with an owner login"
    );
    sqlx::query(&format!("DROP SCHEMA {other_schema} CASCADE"))
        .execute(&fixture.admin)
        .await
        .expect("drop wrong schema");

    sqlx::query(&format!(
        "UPDATE {}._migrations SET checksum = NULL WHERE version = \
         (SELECT MAX(version) FROM {}._migrations)",
        fixture.schema, fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("simulate missing committed checksum");
    let error = verify_global_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
        .await
        .expect_err("missing checksum must refuse runtime attachment");
    assert!(matches!(
        error,
        RuntimeAttachmentError::Database(DatabaseError::MigrationLedger(
            MigrationLedgerError::MissingChecksum { .. }
        ))
    ));

    sqlx::query(&format!(
        "DELETE FROM {}._migrations WHERE version = \
         (SELECT MAX(version) FROM {}._migrations)",
        fixture.schema, fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("simulate pending migration");
    let error = verify_global_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
        .await
        .expect_err("pending migration must refuse runtime attachment");
    assert!(matches!(
        error,
        RuntimeAttachmentError::Database(DatabaseError::MigrationLedger(
            MigrationLedgerError::PendingMigration { .. }
        ))
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn restricted_runtime_login_cannot_run_owner_bootstrap_or_mutate_ledger() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    fixture.bootstrap().await;
    assert!(
        bootstrap_global_owner(&fixture.runtime, &fixture.owner_config(), true)
            .await
            .is_err()
    );

    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    let attempts = [
        format!(
            "ALTER TABLE {}._migrations ADD COLUMN forbidden TEXT",
            fixture.schema
        ),
        format!("DELETE FROM {}._migrations", fixture.schema),
        format!(
            "UPDATE {}._lineage SET lineage_uuid = lineage_uuid",
            fixture.schema
        ),
        format!("SET ROLE {}", fixture.owner_role),
    ];
    for statement in attempts {
        assert!(
            sqlx::query(&statement).execute(&mut runtime).await.is_err(),
            "restricted runtime unexpectedly executed {statement}"
        );
    }
    drop(runtime);
    fixture.cleanup().await;
}

#[tokio::test]
async fn inherited_raw_postgres_transactions_cannot_verify_a_stale_ledger() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    fixture.bootstrap().await;
    let expected = fixture.expected_identity().await;
    let original_checksum: String = sqlx::query_scalar(&format!(
        "SELECT checksum FROM {}._migrations ORDER BY version DESC LIMIT 1",
        fixture.schema
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("committed latest checksum");

    for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
        let guard = fixture.runtime.guard().await.expect("runtime pool member");
        guard
            .execute(&format!("BEGIN ISOLATION LEVEL {isolation}"), ())
            .await
            .expect("raw BEGIN on pooled member");
        guard
            .query("SELECT 1", ())
            .await
            .expect("pin an inherited transaction snapshot");
        drop(guard);
        let inherited: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_catalog.pg_stat_activity \
             WHERE usename = $1 AND state = 'idle in transaction'",
        )
        .bind(&fixture.runtime_role)
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect pooled raw transaction");
        assert_eq!(
            inherited, 1,
            "raw {isolation} transaction must survive pooling"
        );
        sqlx::query(&format!(
            "UPDATE {}._migrations SET checksum = NULL WHERE version = \
             (SELECT MAX(version) FROM {}._migrations)",
            fixture.schema, fixture.schema
        ))
        .execute(&fixture.admin)
        .await
        .expect("change ledger after raw transaction snapshot");

        assert!(
            verify_global_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
                .await
                .is_err(),
            "{isolation} inherited transaction must not attest a stale ledger"
        );
        sqlx::query(&format!(
            "UPDATE {}._migrations SET checksum = $1 WHERE version = \
             (SELECT MAX(version) FROM {}._migrations)",
            fixture.schema, fixture.schema
        ))
        .bind(&original_checksum)
        .execute(&fixture.admin)
        .await
        .expect("restore ledger for next isolation mode");
    }
    verify_global_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
        .await
        .expect("fresh pooled transaction after rollback");
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_temp_copies_cannot_replace_missing_durable_tables() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    fixture.bootstrap().await;
    let expected = fixture.expected_identity().await;
    let guard = fixture.runtime.guard().await.expect("runtime pool member");
    for table in ["_lineage", "_migrations"] {
        guard
            .execute(
                &format!(
                    "CREATE TEMP TABLE {table} AS SELECT * FROM {}.{table}",
                    fixture.schema
                ),
                (),
            )
            .await
            .expect("valid TEMP copy of durable table");
    }
    drop(guard);

    for table in ["_lineage", "_migrations"] {
        sqlx::query(&format!(
            "ALTER TABLE {}.{table} RENAME TO {table}_hidden",
            fixture.schema
        ))
        .execute(&fixture.admin)
        .await
        .expect("hide durable table while TEMP copy remains");
        assert!(
            verify_global_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
                .await
                .is_err(),
            "TEMP {table} must not stand in for missing durable table"
        );
        sqlx::query(&format!(
            "ALTER TABLE {}.{table}_hidden RENAME TO {table}",
            fixture.schema
        ))
        .execute(&fixture.admin)
        .await
        .expect("restore durable table");
    }
    verify_global_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
        .await
        .expect("restored durable tables verify");
    fixture.cleanup().await;
}

#[tokio::test]
async fn sqlite_owner_bootstrap_and_read_only_verifier_preserve_single_node_behavior() {
    let directory = tempfile::tempdir().expect("SQLite fixture directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("owner-bootstrap.db").display()
    );
    let mut database_config = DatabaseConfig::new(DatabaseDriver::Sqlite, url);
    database_config.pool_size = 1;
    let database = Database::from_config("SQLite owner bootstrap", &database_config)
        .await
        .expect("SQLite database");
    let deployment_uuid = DeploymentUuid(uuid::Uuid::new_v4());
    let owner_config = LineageConfig {
        deployment_uuid: Some(deployment_uuid),
        action: Some(LineageAction::Enroll),
    };
    bootstrap_global_owner(&database, &owner_config, false)
        .await
        .expect("SQLite owner bootstrap");
    let runtime_config = LineageConfig {
        deployment_uuid: Some(deployment_uuid),
        action: None,
    };
    let expected = lineage::verify(&database, &runtime_config)
        .await
        .expect("committed SQLite lineage");
    let verified = verify_global_runtime(&database, &runtime_config, &expected)
        .await
        .expect("SQLite runtime verifier");
    assert_eq!(verified, expected);

    let guard = database.guard().await.expect("single SQLite pool member");
    for table in ["_lineage", "_migrations"] {
        guard
            .execute(
                &format!("CREATE TEMP TABLE {table} AS SELECT * FROM main.{table}"),
                (),
            )
            .await
            .expect("valid TEMP copy on verifier connection");
    }
    guard
        .execute(
            &format!(
                "UPDATE main._lineage SET lineage_uuid = '{}'",
                uuid::Uuid::new_v4()
            ),
            (),
        )
        .await
        .expect("change durable lineage while TEMP copy remains valid");
    drop(guard);
    assert!(
        verify_global_runtime(&database, &runtime_config, &expected)
            .await
            .is_err(),
        "TEMP lineage must not hide changed main lineage"
    );

    let guard = database.guard().await.expect("single SQLite pool member");
    guard
        .execute(
            &format!(
                "UPDATE main._lineage SET lineage_uuid = '{}'",
                expected.lineage_uuid.0
            ),
            (),
        )
        .await
        .expect("restore durable lineage");
    guard
        .execute(
            "UPDATE main._migrations SET checksum = NULL WHERE version = \
             (SELECT MAX(version) FROM main._migrations)",
            (),
        )
        .await
        .expect("change durable ledger while TEMP copy remains valid");
    drop(guard);
    assert!(
        verify_global_runtime(&database, &runtime_config, &expected)
            .await
            .is_err(),
        "TEMP ledger must not hide changed main ledger"
    );
}

#[tokio::test]
async fn store_owner_and_runtime_boundaries_work_without_a_migration_ledger() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    fixture.bootstrap_store().await;
    let expected = fixture.expected_identity().await;
    let attested =
        verify_store_lineage_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
            .await
            .expect("restricted lineage-only attachment");
    assert_eq!(attested, expected);
    let migration_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = '_migrations'",
    )
    .bind(&fixture.schema)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect store schema");
    assert_eq!(migration_tables, 0, "store verifier needs no global ledger");
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    assert!(sqlx::query(&format!(
        "UPDATE {}._lineage SET lineage_uuid = lineage_uuid",
        fixture.schema
    ))
    .execute(&mut runtime)
    .await
    .is_err());
    drop(runtime);

    let wrong = LineageConfig {
        deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
        action: None,
    };
    assert!(
        verify_store_lineage_runtime(&fixture.runtime, &wrong, &expected)
            .await
            .is_err()
    );
    assert!(matches!(
        verify_store_lineage_runtime(&fixture.runtime, &fixture.owner_config(), &expected).await,
        Err(RuntimeAttachmentError::OperatorActionConfigured)
    ));
    let mut wrong_expected = expected.clone();
    wrong_expected.lineage_uuid = LineageUuid::new();
    assert!(verify_store_lineage_runtime(
        &fixture.runtime,
        &fixture.runtime_config(),
        &wrong_expected
    )
    .await
    .is_err());
    let mut incomplete = expected.clone();
    incomplete.postgres_identity = None;
    assert!(matches!(
        verify_store_lineage_runtime(&fixture.runtime, &fixture.runtime_config(), &incomplete)
            .await,
        Err(RuntimeAttachmentError::MissingExpectedPostgresIdentity)
    ));

    let other_schema = format!("waddle_boot_x_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(
        "CREATE SCHEMA {other_schema} AUTHORIZATION {}",
        fixture.owner_role
    ))
    .execute(&fixture.admin)
    .await
    .expect("second owner schema");
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {other_schema} TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.admin)
    .await
    .expect("second schema usage");
    let wrong_url = url_with_schema_user(&fixture.base_url, &other_schema, &fixture.runtime_role);
    let wrong_db = open_database("wrong-store", &wrong_url).await;
    assert!(
        verify_store_lineage_runtime(&wrong_db, &fixture.runtime_config(), &expected)
            .await
            .is_err()
    );
    drop(wrong_db);
    sqlx::query(&format!("DROP SCHEMA {other_schema} CASCADE"))
        .execute(&fixture.admin)
        .await
        .expect("drop second schema");

    let guard = fixture.runtime.guard().await.expect("runtime pool member");
    guard
        .execute(
            &format!(
                "CREATE TEMP TABLE _lineage AS SELECT * FROM {}._lineage",
                fixture.schema
            ),
            (),
        )
        .await
        .expect("copy durable lineage into TEMP");
    drop(guard);
    sqlx::query(&format!(
        "ALTER TABLE {}._lineage RENAME TO _lineage_hidden",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("hide durable lineage");
    assert!(
        verify_store_lineage_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
            .await
            .is_err(),
        "TEMP copy cannot substitute for durable lineage"
    );
    sqlx::query(&format!(
        "ALTER TABLE {}._lineage_hidden RENAME TO _lineage",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore durable lineage");
    fixture.cleanup().await;
}

#[tokio::test]
async fn store_runtime_rejects_an_inherited_stale_postgres_snapshot() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    fixture.bootstrap_store().await;
    let expected = fixture.expected_identity().await;
    let guard = fixture.runtime.guard().await.expect("runtime pool member");
    guard
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ", ())
        .await
        .expect("pin raw transaction");
    guard.query("SELECT 1", ()).await.expect("pin snapshot");
    drop(guard);
    sqlx::query(&format!(
        "UPDATE {}._lineage SET lineage_uuid = $1",
        fixture.schema
    ))
    .bind(LineageUuid::new().0.to_string())
    .execute(&fixture.admin)
    .await
    .expect("change committed lineage after snapshot");
    assert!(
        verify_store_lineage_runtime(&fixture.runtime, &fixture.runtime_config(), &expected)
            .await
            .is_err(),
        "stale inherited transaction cannot attest prior lineage"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn store_owner_adopts_two_distinct_postgres_schemas_independently() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    fixture.bootstrap_store().await;
    let first = fixture.expected_identity().await;
    let second_schema = format!("waddle_boot_x_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(
        "CREATE SCHEMA {second_schema} AUTHORIZATION {}",
        fixture.owner_role
    ))
    .execute(&fixture.admin)
    .await
    .expect("second owner schema");
    let second_url = url_with_schema_user(&fixture.base_url, &second_schema, &fixture.owner_role);
    let second_db = open_database("second-store", &second_url).await;
    bootstrap_store_owner(&second_db, &fixture.owner_config())
        .await
        .expect("enroll second boundary");
    let second = lineage::verify(&second_db, &fixture.runtime_config())
        .await
        .expect("second committed identity");
    assert_ne!(first.lineage_uuid, second.lineage_uuid);
    let adoption = LineageConfig {
        deployment_uuid: Some(fixture.deployment_uuid),
        action: Some(LineageAction::Adopt(vec![
            first.lineage_uuid,
            second.lineage_uuid,
        ])),
    };
    assert_eq!(
        bootstrap_store_owner(&fixture.owner, &adoption)
            .await
            .expect("adopt first boundary"),
        Some(first.lineage_uuid)
    );
    assert_eq!(
        bootstrap_store_owner(&second_db, &adoption)
            .await
            .expect("adopt second boundary"),
        Some(second.lineage_uuid)
    );
    assert_eq!(
        bootstrap_store_owner(&fixture.owner, &adoption)
            .await
            .expect("first no longer matches"),
        None
    );
    assert_eq!(
        bootstrap_store_owner(&second_db, &adoption)
            .await
            .expect("second no longer matches"),
        None
    );
    assert_ne!(
        lineage::verify(&fixture.owner, &fixture.runtime_config())
            .await
            .expect("first rotated identity")
            .lineage_uuid,
        first.lineage_uuid
    );
    assert_ne!(
        lineage::verify(&second_db, &fixture.runtime_config())
            .await
            .expect("second rotated identity")
            .lineage_uuid,
        second.lineage_uuid
    );
    drop(second_db);
    sqlx::query(&format!("DROP SCHEMA {second_schema} CASCADE"))
        .execute(&fixture.admin)
        .await
        .expect("drop second schema");
    fixture.cleanup().await;
}

#[tokio::test]
async fn interrupted_store_table_bootstrap_refuses_runtime_until_enrollment() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let no_action = fixture.runtime_config();
    bootstrap_store_owner(&fixture.owner, &no_action)
        .await
        .expect("owner creates lineage table only");
    sqlx::query(&format!(
        "GRANT SELECT ON TABLE {}._lineage TO {}",
        fixture.schema, fixture.runtime_role
    ))
    .execute(&fixture.admin)
    .await
    .expect("runtime read grant");
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {} TO {}",
        fixture.schema, fixture.runtime_role
    ))
    .execute(&fixture.admin)
    .await
    .expect("runtime schema usage");
    let expected = AttestedLineage {
        lineage_uuid: LineageUuid::new(),
        deployment_uuid: fixture.deployment_uuid,
        postgres_identity: Some(
            lineage::live_postgres_identity(&fixture.owner)
                .await
                .expect("owner reads physical identity"),
        ),
    };
    assert!(
        verify_store_lineage_runtime(&fixture.runtime, &no_action, &expected)
            .await
            .is_err(),
        "table without a committed row cannot attach"
    );
    bootstrap_store_owner(&fixture.owner, &fixture.owner_config())
        .await
        .expect("owner retries enrollment");
    let committed = fixture.expected_identity().await;
    assert_eq!(
        verify_store_lineage_runtime(&fixture.runtime, &no_action, &committed)
            .await
            .expect("attach after owner retry"),
        committed
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn sqlite_store_owner_rotates_a_shared_boundary_once() {
    let directory = tempfile::tempdir().expect("SQLite fixture directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("store-bootstrap.db").display()
    );
    let mut database_config = DatabaseConfig::new(DatabaseDriver::Sqlite, url);
    database_config.pool_size = 1;
    let database = Database::from_config("SQLite store", &database_config)
        .await
        .expect("SQLite database");
    let deployment_uuid = DeploymentUuid(uuid::Uuid::new_v4());
    let enroll = LineageConfig {
        deployment_uuid: Some(deployment_uuid),
        action: Some(LineageAction::Enroll),
    };
    assert_eq!(
        bootstrap_store_owner(&database, &enroll)
            .await
            .expect("enroll"),
        None
    );
    let runtime = LineageConfig {
        deployment_uuid: Some(deployment_uuid),
        action: None,
    };
    let original = lineage::verify(&database, &runtime)
        .await
        .expect("read committed lineage");
    assert_eq!(
        verify_store_lineage_runtime(&database, &runtime, &original)
            .await
            .expect("read-only store attachment"),
        original
    );
    let adoption = LineageConfig {
        deployment_uuid: Some(deployment_uuid),
        action: Some(LineageAction::Adopt(vec![original.lineage_uuid])),
    };
    assert_eq!(
        bootstrap_store_owner(&database, &adoption)
            .await
            .expect("first label rotates"),
        Some(original.lineage_uuid)
    );
    assert_eq!(
        bootstrap_store_owner(&database, &adoption)
            .await
            .expect("shared second label skips"),
        None
    );
    assert!(verify_store_lineage_runtime(&database, &runtime, &original)
        .await
        .is_err());
    let rotated = lineage::verify(&database, &runtime)
        .await
        .expect("read rotated lineage");
    assert_ne!(rotated.lineage_uuid, original.lineage_uuid);
    assert_eq!(
        verify_store_lineage_runtime(&database, &runtime, &rotated)
            .await
            .expect("verify rotated boundary"),
        rotated
    );

    let guard = database.guard().await.expect("SQLite pool member");
    guard
        .execute(
            "CREATE TEMP TABLE _lineage AS SELECT * FROM main._lineage",
            (),
        )
        .await
        .expect("TEMP shadow");
    guard
        .execute(
            &format!(
                "UPDATE main._lineage SET lineage_uuid = '{}'",
                LineageUuid::new().0
            ),
            (),
        )
        .await
        .expect("change durable lineage");
    drop(guard);
    assert!(
        verify_store_lineage_runtime(&database, &runtime, &rotated)
            .await
            .is_err(),
        "TEMP shadow cannot hide changed durable lineage"
    );
}

#[tokio::test]
async fn portable_sm_schema_owner_requires_a_schema_writer() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {} TO {}",
        fixture.schema, fixture.runtime_role
    ))
    .execute(&fixture.admin)
    .await
    .expect("runtime may use but not create in owner schema");
    assert!(
        bootstrap_portable_sm_owner_schema(&fixture.runtime)
            .await
            .is_err(),
        "restricted login cannot create the portable SM schema"
    );
    bootstrap_portable_sm_owner_schema(&fixture.owner)
        .await
        .expect("owner provisions portable SM schema");
    bootstrap_portable_sm_owner_schema(&fixture.owner)
        .await
        .expect("owner bootstrap is repeatable");
    assert!(
        bootstrap_portable_sm_owner_schema(&fixture.runtime)
            .await
            .is_err(),
        "restricted login cannot repair an existing SM schema"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn pending_delivery_schema_owner_requires_a_schema_writer() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {} TO {}",
        fixture.schema, fixture.runtime_role
    ))
    .execute(&fixture.admin)
    .await
    .expect("runtime schema usage");
    assert!(
        bootstrap_pending_delivery_owner_schema(&fixture.runtime)
            .await
            .is_err(),
        "restricted login cannot create pending-delivery schema"
    );
    bootstrap_pending_delivery_owner_schema(&fixture.owner)
        .await
        .expect("owner provisions pending-delivery schema");
    bootstrap_pending_delivery_owner_schema(&fixture.owner)
        .await
        .expect("owner bootstrap is repeatable");
    assert!(
        bootstrap_pending_delivery_owner_schema(&fixture.runtime)
            .await
            .is_err(),
        "restricted login cannot repair pending-delivery schema"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn pending_delivery_owner_retries_partial_sqlite_schema_and_preserves_claim_reset() {
    let directory = tempfile::tempdir().expect("SQLite fixture directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("pending-bootstrap.db").display()
    );
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, url);
    config.pool_size = 1;
    let db = Database::from_config("pending owner SQLite", &config)
        .await
        .expect("SQLite database");
    let guard = db.guard().await.expect("SQLite pool member");
    guard.execute(
        "CREATE TABLE pending_delivery (row_id TEXT PRIMARY KEY, recipient_jid TEXT NOT NULL, original_receipt_at INTEGER NOT NULL, payload_kind TEXT NOT NULL, archive_stanza_by TEXT, archive_stanza_id TEXT, transient_xml TEXT, flushed_in_session TEXT)",
        (),
    ).await.expect("legacy pending table");
    guard.execute(
        "INSERT INTO pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, transient_xml, flushed_in_session) VALUES ('legacy-row', 'alice@example.test', 1, 'transient', '<message xmlns=\"jabber:client\"><body>hello</body></message>', 'discarded-session')",
        (),
    ).await.expect("legacy claimed row");
    drop(guard);

    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner repairs partial schema");
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner retry is repeatable");
    let guard = db.guard().await.expect("SQLite pool member");
    let mut rows = guard.query(
        "SELECT flushed_in_session, notification_recovery_ordinal FROM pending_delivery WHERE row_id = 'legacy-row'",
        (),
    ).await.expect("read repaired row");
    let row = rows.next().await.expect("read row").expect("row retained");
    assert_eq!(row.get::<Option<String>>(0).expect("claim"), None);
    assert!(row.get::<i64>(1).expect("recovery ordinal") > 0);
    drop(rows);
    let mut markers = guard
        .query(
            "SELECT name FROM pending_delivery_startup_migrations ORDER BY name",
            (),
        )
        .await
        .expect("read markers");
    let mut names = Vec::new();
    while let Some(row) = markers.next().await.expect("next marker") {
        names.push(row.get::<String>(0).expect("marker name"));
    }
    assert_eq!(
        names,
        [
            "ingress_v1014_pending_claim_reset_v1",
            "legacy_mam_query_frames_v1"
        ]
    );
    drop(markers);
    drop(guard);
    drop(db);

    let reopened = Database::from_config("reopened pending owner SQLite", &config)
        .await
        .expect("reopened SQLite database");
    bootstrap_pending_delivery_owner_schema(&reopened)
        .await
        .expect("owner bootstrap after reopen");
    let guard = reopened.guard().await.expect("reopened SQLite pool member");
    let mut rows = guard
        .query(
            "SELECT COUNT(*) FROM pending_delivery WHERE row_id = 'legacy-row'",
            (),
        )
        .await
        .expect("read retained row");
    let count: i64 = rows
        .next()
        .await
        .expect("read count")
        .expect("count row")
        .get(0)
        .expect("count");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn inbox_schema_owner_requires_a_schema_writer() {
    let _serial = CATALOG_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    sqlx::query(&format!(
        "GRANT USAGE ON SCHEMA {} TO {}",
        fixture.schema, fixture.runtime_role
    ))
    .execute(&fixture.admin)
    .await
    .expect("runtime schema usage");
    assert!(
        bootstrap_inbox_owner_schema(&fixture.runtime)
            .await
            .is_err(),
        "restricted login cannot create inbox schema"
    );
    bootstrap_inbox_owner_schema(&fixture.owner)
        .await
        .expect("owner provisions inbox schema");
    bootstrap_inbox_owner_schema(&fixture.owner)
        .await
        .expect("owner bootstrap is repeatable");
    assert!(
        bootstrap_inbox_owner_schema(&fixture.runtime)
            .await
            .is_err(),
        "restricted login cannot repair inbox schema"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn inbox_runtime_attachment_checks_existing_sqlite_schema_without_repair() {
    use waddle_xmpp::inbox::storage::InboxStorage;

    let config = DatabaseConfig {
        pool_size: 1,
        ..DatabaseConfig::default()
    };
    let db = Database::from_config("runtime inbox", &config)
        .await
        .expect("SQLite database");
    assert!(matches!(
        DatabaseInboxStorage::attach_existing(db.clone()).await,
        Err(InboxRuntimeAttachError::Relation {
            relation: "inbox_entries",
            ..
        })
    ));
    bootstrap_inbox_owner_schema(&db)
        .await
        .expect("owner prepares inbox");
    let guard = db.guard().await.expect("SQLite connection");
    guard
        .execute(
            "INSERT INTO inbox_entries \
             (user_jid, partner_jid, kind, last_stanza_id, last_updated) \
             VALUES ('alice@example.test', 'bob@example.test', 'direct', 'stanza-1', 1)",
            (),
        )
        .await
        .expect("owner seeds inbox entry");
    drop(guard);
    let storage = DatabaseInboxStorage::attach_existing(db.clone())
        .await
        .expect("attach existing SQLite inbox");
    let user = "alice@example.test".parse().expect("valid user JID");
    let entries = storage.list(&user).await.expect("inbox read");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].partner.to_string(), "bob@example.test");
    drop(storage);

    let guard = db.guard().await.expect("SQLite connection");
    guard
        .execute("CREATE TEMP TABLE Inbox_Entries (shadow TEXT)", ())
        .await
        .expect("create session-local shadow");
    drop(guard);
    assert!(matches!(
        DatabaseInboxStorage::attach_existing(db.clone()).await,
        Err(InboxRuntimeAttachError::SqliteTemporaryShadow)
    ));
    let guard = db.guard().await.expect("SQLite connection");
    guard
        .execute("DROP TABLE temp.inbox_entries", ())
        .await
        .expect("remove temporary shadow");
    drop(guard);
    for (create, remove) in [
        (
            "CREATE TEMP TABLE ingress_effect_intents (shadow TEXT)",
            "DROP TABLE temp.ingress_effect_intents",
        ),
        (
            "CREATE TEMP VIEW Ingress_Effect_Receipts AS SELECT 1 AS shadow",
            "DROP VIEW temp.ingress_effect_receipts",
        ),
    ] {
        let guard = db.guard().await.expect("SQLite connection");
        guard
            .execute(create, ())
            .await
            .expect("create ledger shadow");
        drop(guard);
        assert!(matches!(
            DatabaseInboxStorage::attach_existing(db.clone()).await,
            Err(InboxRuntimeAttachError::SqliteTemporaryShadow)
        ));
        let guard = db.guard().await.expect("SQLite connection");
        guard
            .execute(remove, ())
            .await
            .expect("remove ledger shadow");
    }
    DatabaseInboxStorage::attach_existing(db.clone())
        .await
        .expect("attach after removing ledger shadows");
    let guard = db.guard().await.expect("SQLite connection");
    guard
        .execute("DROP INDEX idx_inbox_entries_user_updated", ())
        .await
        .expect("remove required index");
    drop(guard);
    assert!(matches!(
        DatabaseInboxStorage::attach_existing(db.clone()).await,
        Err(InboxRuntimeAttachError::MissingIndex {
            table: "inbox_entries",
            index: "idx_inbox_entries_user_updated"
        })
    ));
    let guard = db.guard().await.expect("SQLite connection");
    let mut rows = guard
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'index' \
             AND name = 'idx_inbox_entries_user_updated'",
            (),
        )
        .await
        .expect("inspect missing index");
    assert!(rows.next().await.expect("index result").is_none());

    let partial = Database::in_memory("partial inbox")
        .await
        .expect("partial SQLite database");
    let guard = partial.guard().await.expect("partial SQLite connection");
    guard
        .execute(
            "CREATE TABLE inbox_entries (user_jid TEXT, partner_jid TEXT)",
            (),
        )
        .await
        .expect("create partially provisioned relation");
    drop(guard);
    assert!(matches!(
        DatabaseInboxStorage::attach_existing(partial).await,
        Err(InboxRuntimeAttachError::Relation {
            relation: "inbox_entries",
            ..
        })
    ));
}

#[tokio::test]
async fn inbox_owner_bootstraps_fresh_sqlite_and_reopens() {
    let directory = tempfile::tempdir().expect("SQLite fixture directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("fresh-inbox.db").display()
    );
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, url);
    config.pool_size = 1;
    let db = Database::from_config("fresh inbox", &config)
        .await
        .expect("SQLite database");
    bootstrap_inbox_owner_schema(&db)
        .await
        .expect("fresh owner bootstrap");
    bootstrap_inbox_owner_schema(&db)
        .await
        .expect("repeat owner bootstrap");
    let guard = db.guard().await.expect("SQLite pool member");
    guard.execute(
        "INSERT INTO inbox_entries (user_jid, partner_jid, kind, last_stanza_id, last_updated) VALUES ('alice@example.test', 'bob@example.test', 'direct', 'stanza-1', 1)",
        (),
    ).await.expect("schema accepts inbox row");
    drop(guard);
    drop(db);

    let reopened = Database::from_config("reopened inbox", &config)
        .await
        .expect("reopened SQLite database");
    bootstrap_inbox_owner_schema(&reopened)
        .await
        .expect("owner bootstrap after reopen");
    let guard = reopened.guard().await.expect("reopened SQLite pool member");
    let mut rows = guard
        .query(
            "SELECT COUNT(*) FROM inbox_entries WHERE last_stanza_id = 'stanza-1'",
            (),
        )
        .await
        .expect("read retained inbox row");
    let count: i64 = rows
        .next()
        .await
        .expect("read count")
        .expect("count row")
        .get(0)
        .expect("count");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn inbox_owner_rebuilds_legacy_threads_and_discards_unreceiptable_recovery() {
    let directory = tempfile::tempdir().expect("SQLite fixture directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("legacy-inbox.db").display()
    );
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, url);
    config.pool_size = 1;
    let db = Database::from_config("legacy inbox", &config)
        .await
        .expect("SQLite database");
    let guard = db.guard().await.expect("SQLite pool member");
    guard.execute(
        "CREATE TABLE inbox_entries (user_jid TEXT NOT NULL, partner_jid TEXT NOT NULL, kind TEXT NOT NULL, last_stanza_id TEXT NOT NULL, last_updated INTEGER NOT NULL, unread INTEGER NOT NULL DEFAULT 0, preview TEXT, PRIMARY KEY (user_jid, partner_jid))",
        (),
    ).await.expect("legacy inbox table");
    guard.execute(
        "INSERT INTO inbox_entries (user_jid, partner_jid, kind, last_stanza_id, last_updated, unread, preview) VALUES ('alice@example.test', 'bob@example.test', 'direct', 'legacy-stanza', 7, 2, 'hello')",
        (),
    ).await.expect("legacy inbox row");
    guard.execute(
        "CREATE TABLE groupchat_notification_recovery (recipient_bare_jid TEXT NOT NULL, room_jid TEXT NOT NULL, thread_id TEXT NOT NULL DEFAULT '', stanza_id_by TEXT NOT NULL, stanza_id TEXT NOT NULL, sender_jid TEXT NOT NULL, is_live_occupant INTEGER NOT NULL, room_members_only INTEGER NOT NULL, created_at_ms INTEGER NOT NULL, completed_at_ms INTEGER, PRIMARY KEY (recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id))",
        (),
    ).await.expect("legacy recovery table");
    guard.execute(
        "INSERT INTO groupchat_notification_recovery (recipient_bare_jid, room_jid, stanza_id_by, stanza_id, sender_jid, is_live_occupant, room_members_only, created_at_ms) VALUES ('alice@example.test', 'room@example.test', 'room@example.test', 'legacy-stanza', 'room@example.test/alice', 1, 0, 7)",
        (),
    ).await.expect("unreceiptable legacy recovery");
    drop(guard);

    bootstrap_inbox_owner_schema(&db)
        .await
        .expect("owner repairs legacy inbox and recovery");
    bootstrap_inbox_owner_schema(&db)
        .await
        .expect("repeat owner repair");
    let guard = db.guard().await.expect("SQLite pool member");
    let mut rows = guard.query(
        "SELECT thread_id, last_stanza_id, unread, preview FROM inbox_entries WHERE user_jid = 'alice@example.test'",
        (),
    ).await.expect("read migrated inbox row");
    let row = rows
        .next()
        .await
        .expect("read inbox row")
        .expect("inbox row retained");
    assert_eq!(row.get::<String>(0).expect("thread"), "");
    assert_eq!(row.get::<String>(1).expect("stanza"), "legacy-stanza");
    assert_eq!(row.get::<i64>(2).expect("unread"), 2);
    assert_eq!(row.get::<String>(3).expect("preview"), "hello");
    drop(rows);
    let mut recovery = guard
        .query("SELECT COUNT(*) FROM groupchat_notification_recovery", ())
        .await
        .expect("count recovery rows");
    let count: i64 = recovery
        .next()
        .await
        .expect("read count")
        .expect("count row")
        .get(0)
        .expect("count");
    assert_eq!(count, 0, "legacy row without canonical key is discarded");
    drop(recovery);
    let mut columns = guard
        .query("PRAGMA table_info(groupchat_notification_recovery)", ())
        .await
        .expect("recovery columns");
    let mut names = Vec::new();
    while let Some(row) = columns.next().await.expect("next column") {
        names.push(row.get::<String>(1).expect("column name"));
    }
    assert!(names.contains(&"sender_can_broadcast_channel_mention".to_owned()));
    assert!(names.contains(&"message_key".to_owned()));
}
