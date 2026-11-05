//! Restricted PostgreSQL attachment for the call-teardown outbox.

use super::{
    attested_pool, install, test_url, url_with_schema, Database, DatabaseConfig, DatabaseDriver,
    Duration, Fixture, CATALOG_GRANT_LOCK,
};
use waddle_server::call_teardown_outbox::{
    CallTeardownOutboxError, CallTeardownOutboxStore, CallTeardownRuntimeAttachError,
};
use waddle_server::db::lineage::PgIdentity;
use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

const LEGACY_SCHEMA: &str =
    include_str!("../../src/call_teardown_outbox/tests/fixtures/pre_displaced_retirement.sql");

async fn run_admin(fixture: &Fixture, statement: &str) {
    sqlx::query(statement)
        .execute(&fixture.admin)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

fn owner_url(fixture: &Fixture, owner: &str, migrator: &str) -> String {
    let mut url = url::Url::parse(&fixture.base_url).expect("base URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "user" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("user", migrator)
        .append_pair(
            "options",
            &format!("-c role={owner} -c search_path={},pg_temp", fixture.target),
        );
    url.to_string()
}

fn node_identity() -> SharedNodeIdentity {
    SharedNodeIdentity::new(NodeIdentity::new("call-teardown-test", "epoch-1"))
}

fn runtime_error(error: CallTeardownOutboxError) -> CallTeardownRuntimeAttachError {
    let CallTeardownOutboxError::RuntimeAttach(error) = error else {
        panic!("expected runtime attach error, got {error}");
    };
    error
}

fn legacy_postgres_schema() -> String {
    LEGACY_SCHEMA
        .replace(
            "thread_started_at_ms INTEGER",
            "thread_started_at_ms BIGINT",
        )
        .replace("thread_ended_at_ms INTEGER", "thread_ended_at_ms BIGINT")
        .replace("next_attempt_at_ms INTEGER", "next_attempt_at_ms BIGINT")
        .replace("claimed_at_ms INTEGER", "claimed_at_ms BIGINT")
        .replace("created_at_ms INTEGER", "created_at_ms BIGINT")
        .replace("updated_at_ms INTEGER", "updated_at_ms BIGINT")
}

#[tokio::test]
async fn call_teardown_attaches_only_to_prepared_restricted_main_pool() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let owner = format!("ct_owner_{}", uuid::Uuid::new_v4().simple());
    let migrator = format!("ct_migrator_{}", uuid::Uuid::new_v4().simple());
    run_admin(&fixture, &format!("CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS")).await;
    run_admin(&fixture, &format!("CREATE ROLE {migrator} LOGIN")).await;
    run_admin(
        &fixture,
        &format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
    )
    .await;
    run_admin(
        &fixture,
        &format!("ALTER SCHEMA {} OWNER TO {owner}", fixture.target),
    )
    .await;
    run_admin(
        &fixture,
        &format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
    )
    .await;

    let pg_identity: PgIdentity = fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("lineage carries physical identity")
        .clone();
    let mut owner_config = DatabaseConfig::new(
        DatabaseDriver::Postgres,
        owner_url(&fixture, &owner, &migrator),
    );
    owner_config.pool_size = 1;
    let owner_db = Database::from_config("call teardown trusted owner", &owner_config)
        .await
        .expect("trusted owner database");

    let attestation = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install physical attestor");
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "call teardown main and ingress",
        attested_pool(
            fixture.runtime_options.clone(),
            attestation,
            2,
            Duration::from_secs(3),
        )
        .expect("restricted attested pool"),
        1,
        1,
    )
    .await
    .expect("main and ingress lanes");
    let runtime = pool.global().clone();
    let node_identity = node_identity();

    let error = CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
        .await
        .err()
        .expect("missing owner schema must fail closed");
    assert!(matches!(
        runtime_error(error),
        CallTeardownRuntimeAttachError::Relation(_)
    ));
    let missing: Option<String> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
        .bind(format!("{}.call_teardown_outbox", fixture.target))
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect missing relation");
    assert!(
        missing.is_none(),
        "runtime attachment must not create tables"
    );
    assert!(matches!(
        CallTeardownOutboxStore::try_new(runtime.clone()).await,
        Err(CallTeardownOutboxError::OwnerPreparationRequired)
    ));

    let mut wrong_identity = pg_identity.clone();
    wrong_identity.database.oid += 1;
    assert!(
        CallTeardownOutboxStore::initialize_owner(&owner_db, &wrong_identity)
            .await
            .is_err()
    );
    let still_missing: Option<String> =
        sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
            .bind(format!("{}.call_teardown_outbox", fixture.target))
            .fetch_one(&fixture.admin)
            .await
            .expect("reinspect relation after wrong identity");
    assert!(
        still_missing.is_none(),
        "wrong identity must not mutate schema"
    );

    // A partial owner upgrade must roll back all preceding column additions.
    owner_db
        .guard()
        .await
        .expect("owner connection")
        .execute(
            "CREATE TABLE call_teardown_outbox (intent_id TEXT PRIMARY KEY, call_id INTEGER NOT NULL)",
            (),
        )
        .await
        .expect("malformed owner fixture");
    assert!(
        CallTeardownOutboxStore::initialize_owner(&owner_db, &pg_identity)
            .await
            .is_err()
    );
    let malformed_column_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = 'call_teardown_outbox' \
           AND a.attnum > 0 AND NOT a.attisdropped",
    )
    .bind(&fixture.target)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect rolled-back columns");
    assert_eq!(malformed_column_count, 2, "owner preparation is atomic");
    run_admin(
        &fixture,
        &format!("DROP TABLE {}.call_teardown_outbox", fixture.target),
    )
    .await;

    // Prepare from the supported pre-#1732 shape and retain an existing row.
    let old_schema = legacy_postgres_schema();
    owner_db
        .guard()
        .await
        .expect("owner connection")
        .execute(&old_schema, ())
        .await
        .expect("create legacy outbox schema");
    owner_db
        .guard()
        .await
        .expect("owner connection")
        .execute(
            "INSERT INTO call_teardown_outbox \
             (intent_id, call_id, identity, action, status, created_at_ms, updated_at_ms) \
             VALUES ('retained-intent', 'retained-call', 'alice@example.test/device', \
                     'remove_participant', 'queued', 1, 1)",
            (),
        )
        .await
        .expect("seed legacy durable intent");
    CallTeardownOutboxStore::initialize_owner(&owner_db, &pg_identity)
        .await
        .expect("upgrade owner schema atomically");
    let retained: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*)::bigint FROM {}.call_teardown_outbox WHERE intent_id = 'retained-intent'",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect preserved intent");
    assert_eq!(retained, 1, "schema upgrade preserves outbox rows");
    run_admin(
        &fixture,
        &format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.call_teardown_outbox TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;

    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(pool.ingress(), node_identity.clone())
                .await
                .err()
                .expect("ingress lane cannot attach")
        ),
        CallTeardownRuntimeAttachError::MainLaneRequired
    ));
    let attached = CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
        .await
        .expect("restricted main lane attaches");
    assert_eq!(
        attached
            .queue_stats()
            .await
            .expect("functional store can read the outbox")
            .queued_count,
        1
    );
    assert!(runtime
        .guard()
        .await
        .expect("runtime connection")
        .execute("CREATE TABLE runtime_ddl_probe (id integer)", ())
        .await
        .is_err());

    run_admin(
        &fixture,
        &format!(
            "CREATE INDEX call_teardown_unexpected_idx ON {}.call_teardown_outbox (created_at_ms)",
            fixture.target
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("extra index must be refused")
        ),
        CallTeardownRuntimeAttachError::InvalidIndexes
    ));
    run_admin(
        &fixture,
        &format!("DROP INDEX {}.call_teardown_unexpected_idx", fixture.target),
    )
    .await;

    run_admin(
        &fixture,
        &format!(
            "ALTER TABLE {}.call_teardown_outbox DROP CONSTRAINT call_teardown_outbox_target_check",
            fixture.target
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("missing target check must be refused")
        ),
        CallTeardownRuntimeAttachError::InvalidConstraints
    ));
    CallTeardownOutboxStore::initialize_owner(&owner_db, &pg_identity)
        .await
        .expect("owner upgrade restores the supported check contract");

    run_admin(
        &fixture,
        &format!(
            "CREATE TABLE {}.call_teardown_inbound_ref (intent_id text \
             REFERENCES {}.call_teardown_outbox(intent_id) ON DELETE CASCADE)",
            fixture.target, fixture.target
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("inbound cascade foreign key must be refused")
        ),
        CallTeardownRuntimeAttachError::InvalidTable
    ));
    run_admin(
        &fixture,
        &format!("DROP TABLE {}.call_teardown_inbound_ref", fixture.target),
    )
    .await;

    run_admin(
        &fixture,
        &format!(
            "GRANT TRUNCATE ON {}.call_teardown_outbox TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("runtime truncate privilege must be refused")
        ),
        CallTeardownRuntimeAttachError::UnsafeRuntimePrivileges
    ));
    run_admin(
        &fixture,
        &format!(
            "REVOKE TRUNCATE ON {}.call_teardown_outbox FROM {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    run_admin(
        &fixture,
        &format!(
            "GRANT CREATE ON SCHEMA {} TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("runtime schema-create privilege must be refused")
        ),
        CallTeardownRuntimeAttachError::UnsafeRuntimeRole
    ));
    run_admin(
        &fixture,
        &format!(
            "REVOKE CREATE ON SCHEMA {} FROM {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    run_admin(
        &fixture,
        &format!(
            "GRANT SELECT ON {}.call_teardown_outbox TO {}",
            fixture.target, fixture.outsider
        ),
    )
    .await;
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
                .await
                .err()
                .expect("unexpected relation grant must be refused")
        ),
        CallTeardownRuntimeAttachError::UnsafeRuntimePrivileges
    ));
    run_admin(
        &fixture,
        &format!(
            "REVOKE SELECT ON {}.call_teardown_outbox FROM {}",
            fixture.target, fixture.outsider
        ),
    )
    .await;

    // A shape change held open by owner DDL must block the attach lock. Once
    // committed, attachment sees the new snapshot and rejects the bad shape.
    let mut ddl = fixture.admin.begin().await.expect("owner DDL transaction");
    sqlx::query(&format!(
        "ALTER TABLE {}.call_teardown_outbox ALTER COLUMN call_id DROP NOT NULL",
        fixture.target
    ))
    .execute(&mut *ddl)
    .await
    .expect("hold a shape-changing owner DDL lock");
    let attach_task = tokio::spawn({
        let runtime = runtime.clone();
        let node_identity = node_identity.clone();
        async move { CallTeardownOutboxStore::attach_existing(runtime, node_identity).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !attach_task.is_finished(),
        "attachment must wait for conflicting owner DDL"
    );
    ddl.commit().await.expect("commit shape change");
    let race_error = runtime_error(
        attach_task
            .await
            .expect("attachment task")
            .err()
            .expect("attachment sees the committed invalid shape"),
    );
    assert!(matches!(
        race_error,
        CallTeardownRuntimeAttachError::InvalidColumn("call_id")
    ));
    run_admin(
        &fixture,
        &format!(
            "ALTER TABLE {}.call_teardown_outbox ALTER COLUMN call_id SET NOT NULL",
            fixture.target
        ),
    )
    .await;
    CallTeardownOutboxStore::attach_existing(runtime.clone(), node_identity.clone())
        .await
        .expect("restored shape attaches");

    drop(attached);
    drop(owner_db);
    drop(runtime);
    drop(pool);
    run_admin(&fixture, &format!("DROP SCHEMA {} CASCADE", fixture.target)).await;
    run_admin(
        &fixture,
        &format!("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {owner}"),
    )
    .await;
    run_admin(&fixture, &format!("REVOKE {owner} FROM {migrator}")).await;
    run_admin(&fixture, &format!("DROP OWNED BY {owner}")).await;
    run_admin(&fixture, &format!("DROP OWNED BY {migrator}")).await;
    run_admin(&fixture, &format!("DROP ROLE {migrator}")).await;
    run_admin(&fixture, &format!("DROP ROLE {owner}")).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn call_teardown_attachment_rejects_non_postgres_and_unattested_handles() {
    let node_identity = node_identity();
    let sqlite = Database::in_memory("call teardown attach SQLite")
        .await
        .expect("SQLite test database");
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(sqlite.clone(), node_identity.clone())
                .await
                .err()
                .expect("SQLite attach is unsupported")
        ),
        CallTeardownRuntimeAttachError::AttestedPostgresRequired
    ));
    assert!(matches!(
        CallTeardownOutboxStore::try_new(sqlite.clone()).await,
        Ok(_)
    ));

    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let raw = Database::from_config(
        "unattested call teardown",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &fixture.target),
        ),
    )
    .await
    .expect("raw PostgreSQL handle");
    assert!(matches!(
        runtime_error(
            CallTeardownOutboxStore::attach_existing(raw, node_identity)
                .await
                .err()
                .expect("unattested PostgreSQL attach is unsupported")
        ),
        CallTeardownRuntimeAttachError::AttestedPostgresRequired
    ));
    fixture.cleanup().await;
}
