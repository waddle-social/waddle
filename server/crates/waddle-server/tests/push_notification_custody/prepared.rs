//! Successful public-entry custody inspection with real owner-prepared stores.

use super::{run_sql, Fixture};
use sqlx::postgres::PgPool;
use std::time::Duration;
use waddle_server::db::physical_attestation::{attested_pool, install};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::notification_activity::NotificationActivityStore;
use waddle_server::pubsub::DatabasePubSubStorage;
use waddle_server::push_registrations::DatabasePushRegistrationStore;
use waddle_server::server::push_notification_custody::verify_attested_push_notification_custody;
use waddle_server::server::push_notification_owner::prepare_core_push_outbox_vapid_owner;

#[tokio::test]
async fn pg17_prepared_custody_reader_classifies_and_acknowledges_rollback() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(url.clone()).await;
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
        .fetch_one(&f.admin)
        .await
        .expect("PostgreSQL version");
    assert!(
        (170_000..180_000).contains(&version),
        "requires PostgreSQL 17"
    );
    let owner = format!("custody_owner_{}", uuid::Uuid::new_v4().simple());
    let admin_login: String = sqlx::query_scalar("SELECT session_user::text")
        .fetch_one(&f.cluster_admin)
        .await
        .expect("test administrator login");
    run_sql(
        &f,
        &format!("CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT {owner} TO \"{}\" WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
            admin_login.replace('"', "\"\"")
        ),
    )
    .await;
    for statement in [
        format!(
            "REVOKE CREATE, TEMPORARY ON DATABASE {} FROM PUBLIC",
            f.database_name
        ),
        format!("ALTER SCHEMA {} OWNER TO {owner}", f.target),
        format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
    ] {
        run_sql(&f, &statement).await;
    }
    let mut owner_url = url::Url::parse(&f.base_url).expect("owner URL");
    let retained: Vec<_> = owner_url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    owner_url
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair(
            "options",
            &format!("-c role={owner} -c search_path={}", f.target),
        );
    let owner_db = Database::from_config(
        "custody owner",
        &DatabaseConfig::new(DatabaseDriver::Postgres, owner_url.to_string()),
    )
    .await
    .expect("open owner database");
    let identity = f
        .lineage
        .postgres_identity
        .as_ref()
        .expect("PostgreSQL identity");
    DatabasePubSubStorage::initialize_owner(owner_db.clone(), identity)
        .await
        .expect("prepare PubSub");
    prepare_core_push_outbox_vapid_owner(&owner_db, identity, &[7_u8; 32])
        .await
        .expect("prepare Push, outbox, ancestry, and VAPID");
    DatabasePushRegistrationStore::initialize_owner(owner_db.clone())
        .await
        .expect("prepare registration");
    NotificationActivityStore::initialize_owner(owner_db.clone())
        .await
        .expect("prepare activity");
    // The custody classifier needs this Foundation relation; the rest of the
    // Foundation migration and its serving authority are outside this test.
    run_sql(
        &f,
        &format!(
            "CREATE TABLE {}.ingress_effect_descendants (\
             message_key uuid NOT NULL, kind integer NOT NULL, \
             semantic_identity_hash bytea NOT NULL, descendant_key text NOT NULL, \
             settled_at timestamptz)",
            f.target
        ),
    )
    .await;
    grant_runtime(&f).await;
    let config = install(&f.installer_options, f.config(), f.operation_id)
        .await
        .expect("install physical attestor");
    let application = format!("custody_prepared_{}", uuid::Uuid::new_v4().simple());
    let attested = attested_pool(
        f.runtime_options.clone().application_name(&application),
        config,
        1,
        Duration::from_secs(3),
    )
    .expect("one-slot attested pool");
    let mut checkout = attested
        .acquire_for_test()
        .await
        .expect("warm reader checkout");
    let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *checkout)
        .await
        .expect("read reader backend");
    checkout.return_to_pool().await;
    let runtime = waddle_server::test_support::database_from_attested_postgres_pool(
        "prepared custody reader",
        attested,
    );

    let mut foundation = f.admin.begin().await.expect("begin Foundation blocker");
    sqlx::query(&format!(
        "LOCK TABLE {}.ingress_effect_descendants IN ACCESS EXCLUSIVE MODE",
        f.target
    ))
    .execute(&mut *foundation)
    .await
    .expect("hold Foundation relation");
    let db = runtime.clone();
    let reader =
        tokio::spawn(
            async move { verify_attested_push_notification_custody(db, &[7_u8; 32]).await },
        );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
                 WHERE a.pid = $1 AND a.application_name = $2 AND a.xact_start IS NOT NULL \
                   AND a.state = 'active' AND a.wait_event_type = 'Lock' \
                   AND a.query LIKE '%ingress_effect_descendants%' \
                   AND (SELECT count(*) FROM pg_catalog.pg_locks l \
                        WHERE l.pid = a.pid AND l.locktype = 'advisory' AND l.granted \
                          AND l.mode = 'ShareLock') = 3 \
                   AND (SELECT count(*) FROM pg_catalog.pg_locks l \
                        WHERE l.pid = a.pid AND l.locktype = 'advisory' AND l.granted \
                          AND l.mode = 'ExclusiveLock') = 2)",
            )
            .bind(pid)
            .bind(&application)
            .fetch_one(&f.admin)
            .await
            .expect("observe exact operation backend");
            if blocked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("reader reaches Foundation on its warm backend with all five gates");
    foundation
        .rollback()
        .await
        .expect("release Foundation blocker");
    let evidence = tokio::time::timeout(Duration::from_secs(60), reader)
        .await
        .expect("custody read did not complete")
        .expect("join custody reader")
        .expect("owner-prepared custody classifies successfully");
    assert!(format!("{evidence}").contains("outbox[classified=0, unmatched_legacy=0"));

    // No new checkout occurs between the completed public operation and this
    // observation. Its classification query ran after the checkout hook, so
    // this last ROLLBACK is the operation's acknowledged transaction end.
    let idle_after_rollback: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
         WHERE a.pid = $1 AND a.application_name = $2 AND a.state = 'idle' \
           AND a.query = 'ROLLBACK' AND a.xact_start IS NULL \
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
                           WHERE l.pid = a.pid AND l.locktype = 'advisory'))",
    )
    .bind(pid)
    .bind(&application)
    .fetch_one(&f.admin)
    .await
    .expect("inspect operation rollback");
    assert!(
        idle_after_rollback,
        "operation rollback not observed on backend {pid}"
    );

    drop(runtime);
    drop(owner_db);
    f.cleanup().await;
    let cluster = PgPool::connect(&url)
        .await
        .expect("reconnect for owner cleanup");
    sqlx::query(&format!("DROP ROLE {owner}"))
        .execute(&cluster)
        .await
        .expect("drop custody owner role");
    cluster.close().await;
}

async fn grant_runtime(f: &Fixture) {
    for (table, privileges) in [
        ("notification_candidates", "SELECT, INSERT, UPDATE, DELETE"),
        ("notification_outbox", "SELECT, INSERT, UPDATE, DELETE"),
        (
            "notification_outbox_lineage",
            "SELECT, INSERT, UPDATE, DELETE",
        ),
        ("notification_outbox_owner_state", "SELECT"),
        ("push_service_schema_marker", "SELECT"),
        ("push_owner_locks", "SELECT, INSERT, UPDATE"),
        ("push_nodes", "SELECT, INSERT, UPDATE, DELETE"),
        ("push_node_locks", "SELECT, INSERT, UPDATE"),
        ("push_devices", "SELECT, INSERT, UPDATE, DELETE"),
        ("push_delivery_attempts", "SELECT, INSERT, DELETE"),
        ("push_publish_jobs", "SELECT, INSERT, UPDATE, DELETE"),
        ("push_publication_orders", "SELECT, INSERT, UPDATE"),
        ("push_service_vapid_keys", "SELECT"),
        ("push_service_vapid_schema_marker", "SELECT"),
        ("push_registrations", "SELECT, INSERT, UPDATE, DELETE"),
        ("notification_activity", "SELECT, INSERT, UPDATE"),
        ("notification_activity_owner_state", "SELECT"),
        ("pubsub_schema_marker", "SELECT"),
        ("pubsub_schema_version", "SELECT"),
        ("pubsub_nodes", "SELECT, INSERT, UPDATE, DELETE"),
        ("pubsub_items", "SELECT, INSERT, UPDATE, DELETE"),
        ("pubsub_subscriptions", "SELECT, INSERT, DELETE"),
        ("pubsub_affiliations", "SELECT, INSERT, UPDATE, DELETE"),
        ("pubsub_owner_configured", "SELECT, INSERT"),
        ("dnd_projection", "SELECT, INSERT, UPDATE, DELETE"),
        ("dnd_projection_source_version", "SELECT, INSERT, UPDATE"),
        (
            "notification_settings_projection",
            "SELECT, INSERT, UPDATE, DELETE",
        ),
        (
            "notification_settings_projection_source_version",
            "SELECT, INSERT, UPDATE",
        ),
        ("pubsub_push_publications", "SELECT, INSERT, UPDATE"),
        ("ingress_effect_descendants", "SELECT"),
    ] {
        run_sql(
            f,
            &format!(
                "GRANT {privileges} ON {}.{table} TO {}",
                f.target, f.runtime
            ),
        )
        .await;
    }
    run_sql(
        f,
        &format!(
            "GRANT SELECT ON SEQUENCE {}.pubsub_items_seq_seq TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        f,
        &format!(
            "GRANT EXECUTE ON FUNCTION {}.pubsub_runtime_attach_lock() TO {}",
            f.target, f.runtime
        ),
    )
    .await;
}
