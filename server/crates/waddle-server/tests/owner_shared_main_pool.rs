use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;
use waddle_server::config::LineageConfig;
use waddle_server::db::bootstrap::bootstrap_shared_owner_postgres;
use waddle_server::db::{DatabaseConfig, DatabaseDriver};
use waddle_server::mam_runtime::DatabaseMamStorage;
use waddle_server::server::{ResolvedXmppDatabaseUrl, XmppConfig};
use waddle_xmpp::mam::MamStorage;

#[test]
fn shared_owner_locations_refuse_explicit_store_override() {
    let inherited =
        ResolvedXmppDatabaseUrl::InheritedGlobal("postgresql://localhost/waddle".to_owned());
    let mut config = XmppConfig {
        mam_database_url: inherited.clone(),
        inbox_database_url: inherited.clone(),
        pubsub_database_url: inherited.clone(),
        spaces_metadata_database_url: inherited.clone(),
        channel_space_links_database_url: inherited.clone(),
        sm_database_url: inherited.clone(),
        pending_delivery_database_url: inherited,
        ..XmppConfig::default()
    };
    config
        .validate_shared_owner_locations()
        .expect("all stores inherit main");
    config.pubsub_database_url =
        ResolvedXmppDatabaseUrl::Explicit("postgresql://localhost/other".to_owned());
    assert!(config.validate_shared_owner_locations().is_err());
}

#[tokio::test]
async fn owner_mam_and_ingress_use_reserved_lanes_of_one_main_pool() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping owner main-pool test: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let schema = format!("waddle_owner_main_{}", Uuid::new_v4().simple());
    let admin = PgPool::connect(&database_url).await.expect("admin pool");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("isolated schema");
    let mut url = url::Url::parse(&database_url).expect("PostgreSQL URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair("options", &format!("-c search_path={schema}"));

    let config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
    let (pool, _, outbox) = bootstrap_shared_owner_postgres(
        config,
        &LineageConfig::default(),
        false,
        b"test-key",
        1,
        1,
    )
    .await
    .expect("owner bootstraps and seals one pool");
    let main = pool.global().clone();
    let ingress = pool.ingress();
    let mam = DatabaseMamStorage::attach_legacy_owner(&main, false, false)
        .await
        .expect("owner MAM attaches to main lane");

    let main_transaction = main.begin().await.expect("main slot");
    let ingress_transaction = tokio::time::timeout(Duration::from_secs(1), ingress.begin())
        .await
        .expect("main occupancy must not block ingress")
        .expect("ingress slot");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), main.begin())
            .await
            .is_err(),
        "a second main transaction must wait for the main admission permit"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), ingress.begin())
            .await
            .is_err(),
        "a second ingress transaction must wait for the ingress admission permit"
    );
    main_transaction
        .rollback()
        .await
        .expect("release main slot");
    let archive: jid::BareJid = "alice@example.test".parse().expect("archive JID");
    let count = tokio::time::timeout(Duration::from_secs(1), mam.count_messages(&archive))
        .await
        .expect("ingress occupancy must not block standalone MAM")
        .expect("MAM count");
    assert_eq!(count, 0);
    let candidates = tokio::time::timeout(Duration::from_secs(1), outbox.count_all_candidates())
        .await
        .expect("ingress occupancy must not block the prepared outbox")
        .expect("outbox read");
    assert_eq!(candidates, 0);
    let pruned = tokio::time::timeout(Duration::from_secs(1), outbox.prune_completed_before(0, 1))
        .await
        .expect("ingress occupancy must not block outbox writes")
        .expect("outbox write");
    assert_eq!(pruned.total_deleted(), 0);
    ingress_transaction
        .rollback()
        .await
        .expect("release ingress slot");

    drop(mam);
    drop(outbox);
    drop(ingress);
    drop(main);
    drop(pool);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}

#[tokio::test]
async fn malformed_existing_outbox_blocks_owner_lane_seal() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let schema = format!("waddle_owner_bad_outbox_{}", Uuid::new_v4().simple());
    let admin = PgPool::connect(&database_url).await.expect("admin pool");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("isolated schema");
    sqlx::query(&format!(
        "CREATE VIEW {schema}.notification_candidates AS SELECT 1 AS malformed"
    ))
    .execute(&admin)
    .await
    .expect("malformed candidate relation");
    let mut url = url::Url::parse(&database_url).expect("PostgreSQL URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair("options", &format!("-c search_path={schema}"));
    let result = bootstrap_shared_owner_postgres(
        DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string()),
        &LineageConfig::default(),
        false,
        b"test-key",
        1,
        1,
    )
    .await;
    assert!(
        matches!(
            result,
            Err(waddle_server::db::bootstrap::SharedOwnerBootstrapError::NotificationOutbox(_))
        ),
        "malformed preexisting outbox must fail during owner preparation"
    );
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}

#[tokio::test]
async fn explicit_store_override_fails_before_owner_schema_mutation() {
    let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let schema = format!("waddle_owner_override_{}", Uuid::new_v4().simple());
    let admin = PgPool::connect(&database_url).await.expect("admin pool");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("isolated schema");
    let mut url = url::Url::parse(&database_url).expect("PostgreSQL URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair("options", &format!("-c search_path={schema}"));
    let owner_url = url.to_string();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_waddle-server"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("WADDLE_")
            || key.to_string_lossy().starts_with("OTEL_")
        {
            command.env_remove(key);
        }
    }
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        command
            .env("WADDLE_DB_DRIVER", "postgres")
            .env("WADDLE_DATABASE_URL", &owner_url)
            .env("WADDLE_XMPP_PUBSUB_DATABASE_URL", &owner_url)
            .env(
                "WADDLE_SESSION_KEY",
                "integration-test-session-key-32-bytes-long",
            )
            .env(
                "WADDLE_OCCUPANT_ID_SECRET",
                "integration-test-occupant-id-secret-32-bytes-long",
            )
            .env("WADDLE_CERTS_EPHEMERAL", "true")
            .env("WADDLE_HTTP_ADDR", "127.0.0.1:0")
            .env("WADDLE_GIT_SHA", waddle_ws_test_support::TEST_GIT_SHA)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("explicit override exits before startup")
    .expect("run server");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("shared owner PostgreSQL startup requires inherited global PubSub storage"),
        "startup must reach the shared-owner location gate"
    );
    let lineage: Option<String> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
        .bind(format!("{schema}._lineage"))
        .fetch_one(&admin)
        .await
        .expect("inspect lineage relation");
    assert!(
        lineage.is_none(),
        "override must fail before owner mutation"
    );
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}
