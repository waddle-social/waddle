//! Public-entry PostgreSQL witnesses for the Activity catalog fence.
//! The URL must name a disposable PostgreSQL 17 cluster.

use std::time::Duration;

use sqlx::PgPool;
use waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::notification_activity::NotificationActivityStore;

const ACTIVITY_GATE: (i32, i32) = (0x5744444c, 0x4e414354);

fn database_url(base: &str, database: &str, options: &str, application: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "dbname" | "options" | "application_name"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_path(&format!("/{database}"));
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("dbname", database)
        .append_pair("options", options)
        .append_pair("application_name", application);
    url.to_string()
}

async fn marker(admin: &PgPool, schema: &str) -> String {
    sqlx::query_scalar(&format!(
        "SELECT prepared_revision FROM {schema}.notification_activity_owner_state"
    ))
    .fetch_one(admin)
    .await
    .expect("read marker")
}

async fn wait_for<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if check().await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    })
    .await
    .expect("wait for PostgreSQL witness");
}

#[tokio::test]
async fn pg17_activity_public_entries_fence_mutation_and_marker() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let cluster = PgPool::connect(&url).await.expect("connect cluster");
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
        .fetch_one(&cluster)
        .await
        .expect("read version");
    assert!((170_000..180_000).contains(&version), "requires PG17");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let database = format!("activity_fence_{suffix}");
    let owner = format!("activity_o_{suffix}");
    let schema = format!("activity_s_{suffix}");
    let application = format!("activity_fence_{suffix}");
    let login: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(&cluster)
        .await
        .expect("read login");
    for statement in [
        format!("CREATE DATABASE {database}"),
        format!("REVOKE CREATE, TEMPORARY ON DATABASE {database} FROM PUBLIC"),
        format!("CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
        format!("GRANT {owner} TO \"{}\"", login.replace('"', "\"\"")),
    ] {
        sqlx::query(&statement).execute(&cluster).await.expect("prepare fixture");
    }
    let admin_url = database_url(&url, &database, "", "activity_admin");
    let admin = PgPool::connect(&admin_url)
        .await
        .expect("connect fixture database");
    for statement in [
        "REVOKE ALL ON SCHEMA public FROM PUBLIC".to_owned(),
        format!("CREATE SCHEMA {schema} AUTHORIZATION {owner}"),
        format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
    ] {
        sqlx::query(&statement)
            .execute(&admin)
            .await
            .expect("prepare owner schema");
    }
    let owner_url = database_url(
        &url,
        &database,
        &format!("-c role={owner} -c search_path={schema}"),
        &application,
    );
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, owner_url);
    config.pool_size = 1;
    let db = Database::from_config("activity fenced owner", &config)
        .await
        .expect("open one-slot owner handle");
    NotificationActivityStore::initialize_owner(db.clone())
        .await
        .expect("strict owner initializes");
    assert_eq!(marker(&admin, &schema).await, "1");

    // Contention on the lower gate must release catalog-X before the retry.
    // Change the marker while the gate is held: the retry must re-read it.
    let mut gate = admin.begin().await.expect("begin Activity gate holder");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1, $2)")
        .bind(ACTIVITY_GATE.0)
        .bind(ACTIVITY_GATE.1)
        .execute(&mut *gate)
        .await
        .expect("hold Activity gate");
    sqlx::query(&format!(
        "UPDATE {schema}.notification_activity_owner_state SET schema_name = 'wrong'"
    ))
    .execute(&mut *gate)
    .await
    .expect("stage marker drift");
    let task = tokio::spawn({
        let db = db.clone();
        async move { NotificationActivityStore::initialize_owner(db).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let snapshot: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT state::text, query::text, xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE application_name = $1",
    )
    .bind(&application)
    .fetch_all(&admin)
    .await
    .expect("inspect retry snapshot");
    assert!(
        snapshot
            .iter()
            .any(|(state, query, no_xact)| state == "idle" && query == "ROLLBACK" && *no_xact),
        "owner did not acknowledge rollback after Activity gate contention: {snapshot:?}"
    );
    // Take catalog-S only after the owner's ROLLBACK was observed. Holding it
    // pins the release witness and stops a later retry from reacquiring X.
    let mut catalog_reader = admin.begin().await.expect("begin catalog reader");
    tokio::time::timeout(
        Duration::from_secs(3),
        sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock_shared($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .execute(&mut *catalog_reader),
    )
    .await
    .expect("catalog-X released after Activity miss")
    .expect("take catalog-S after Activity miss");
    assert!(!task.is_finished(), "owner retry should remain pending");
    gate.commit()
        .await
        .expect("commit marker drift and release gate");
    catalog_reader.rollback().await.expect("release catalog-S");
    assert!(task.await.expect("join strict owner").is_err());
    sqlx::query(&format!(
        "UPDATE {schema}.notification_activity_owner_state SET schema_name = $1"
    ))
    .bind(&schema)
    .execute(&admin)
    .await
    .expect("restore marker identity");

    // Force the public initializer to wait inside CREATE INDEX. Observe both
    // exclusive advisory locks on that exact backend, then cancel it. The
    // previous marker and missing index must survive the rollback together.
    sqlx::query(&format!(
        "DROP INDEX {schema}.idx_notification_activity_conversation_active"
    ))
    .execute(&admin)
    .await
    .expect("remove activity index");
    let mut blocker = admin.begin().await.expect("begin relation blocker");
    sqlx::query(&format!(
        "LOCK TABLE {schema}.notification_activity IN ACCESS EXCLUSIVE MODE"
    ))
    .execute(&mut *blocker)
    .await
    .expect("block schema mutation");
    let task = tokio::spawn({
        let db = db.clone();
        async move { NotificationActivityStore::initialize_owner(db).await }
    });
    wait_for(|| async {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
             WHERE a.application_name = $1 AND a.state = 'active' \
               AND a.xact_start IS NOT NULL AND a.wait_event_type = 'Lock' \
               AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid \
                 AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 1 \
                 AND l.classid::bigint = ($2::bigint >> 32) \
                 AND l.objid::bigint = ($2::bigint & 4294967295) AND l.mode = 'ExclusiveLock') \
               AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid \
                 AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 2 \
                 AND l.classid::bigint = $3 AND l.objid::bigint = $4 AND l.mode = 'ExclusiveLock'))",
        )
        .bind(&application)
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .bind(i64::from(ACTIVITY_GATE.0))
        .bind(i64::from(ACTIVITY_GATE.1))
        .fetch_one(&admin)
        .await
        .expect("observe mutating backend and both gates")
    })
    .await;
    assert_eq!(marker(&admin, &schema).await, "1");
    task.abort();
    match task.await {
        Err(error) => assert!(error.is_cancelled(), "initializer should be cancelled"),
        Ok(_) => panic!("initializer should be cancelled"),
    }
    blocker.rollback().await.expect("release relation blocker");
    wait_for(|| async {
        let held: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&admin)
            .await
            .expect("probe catalog release");
        held
    })
    .await;
    let index_absent: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NULL")
        .bind(format!(
            "{schema}.idx_notification_activity_conversation_active"
        ))
        .fetch_one(&admin)
        .await
        .expect("inspect index");
    assert!(index_absent);
    assert_eq!(marker(&admin, &schema).await, "1");

    // Raw startup invalidates an existing strict completion in the same commit
    // as its schema work; strict startup may subsequently complete it again.
    NotificationActivityStore::try_new(db.clone())
        .await
        .expect("raw owner initialization");
    assert_eq!(marker(&admin, &schema).await, "0");
    NotificationActivityStore::initialize_owner(db.clone())
        .await
        .expect("strict owner reinitializes");
    assert_eq!(marker(&admin, &schema).await, "1");
    sqlx::query(&format!(
        "UPDATE {schema}.notification_activity_owner_state SET system_identifier = 'wrong'"
    ))
    .execute(&admin)
    .await
    .expect("corrupt marker identity");
    assert!(NotificationActivityStore::try_new(db.clone())
        .await
        .is_err());
    assert_eq!(marker(&admin, &schema).await, "1");

    drop(db);
    admin.close().await;
    sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .execute(&cluster)
        .await
        .expect("drop test database");
    sqlx::query(&format!(
        "REVOKE {owner} FROM \"{}\"",
        login.replace('"', "\"\"")
    ))
    .execute(&cluster)
    .await
    .expect("revoke owner membership");
    sqlx::query(&format!("DROP ROLE {owner}"))
        .execute(&cluster)
        .await
        .expect("drop owner role");
    cluster.close().await;
}
