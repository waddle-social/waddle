//! Real PostgreSQL checks for the dark P2.1 owner/runtime split.

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use waddle_server::config::LineageConfig;
use waddle_server::db::bootstrap::{
    bootstrap_global_owner, verify_global_runtime, RuntimeAttachmentError,
};
use waddle_server::db::lineage::{
    self, AttestedLineage, DeploymentUuid, LineageAction, LineageUuid,
};
use waddle_server::db::{
    Database, DatabaseConfig, DatabaseDriver, DatabaseError, MigrationLedgerError,
};

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
