use super::*;
use crate::auth::{NativeUserStore, RegisterRequest};
use crate::config::ServerConfig;
use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
use crate::permissions::{Object, ObjectType, Permission, Relation, Subject, Tuple, WriteTuple};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::{routing::get, Router};
use base64::prelude::*;
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

static ENV_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_MUTEX
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .expect("env mutex")
}

/// XmppConfig for unit tests: uses in-memory SQLite for all storage backends.
fn test_xmpp_config() -> XmppConfig {
    XmppConfig {
        pubsub_database_url: Some("sqlite::memory:".to_string()),
        ..XmppConfig::default()
    }
}

async fn create_test_state() -> Arc<AppState> {
    let config = DatabaseConfig::default();
    let pool_config = PoolConfig;
    let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

    // Run migrations
    let runner = MigrationRunner::global();
    runner.run(db_pool.global()).await.unwrap();

    Arc::new(AppState::new(Arc::new(db_pool)))
}

#[tokio::test]
async fn extension_pubsub_permission_allows_bootstrap_chat_member() {
    let state = create_test_state().await;
    let subject = Subject::user("user-alice");

    assert!(
        !extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &subject,
            "chat",
            Permission::SendMessage,
        )
        .await
        .expect("initial permission check"),
        "server membership should be required before default chat policy applies"
    );

    state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(
                    ObjectType::Server,
                    bootstrap_membership::DEPLOYMENT_SERVER_ID,
                ),
                Relation::new("member"),
                subject.clone(),
            ),
        })
        .await
        .expect("server member tuple");

    let owner_subject = Subject::user("user-owner");
    state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(
                    ObjectType::Server,
                    bootstrap_membership::DEPLOYMENT_SERVER_ID,
                ),
                Relation::new("owner"),
                owner_subject.clone(),
            ),
        })
        .await
        .expect("server owner tuple");
    let admin_subject = Subject::user("user-admin");
    state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(
                    ObjectType::Server,
                    bootstrap_membership::DEPLOYMENT_SERVER_ID,
                ),
                Relation::new("admin"),
                admin_subject.clone(),
            ),
        })
        .await
        .expect("server admin tuple");

    assert!(
        extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &subject,
            "chat",
            Permission::SendMessage,
        )
        .await
        .expect("chat permission check"),
        "default chat extension publishes should inherit deployment membership"
    );
    assert!(
        extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &subject,
            "github-actions",
            Permission::SendMessage,
        )
        .await
        .expect("github-actions permission check"),
        "github actions alerts should inherit deployment membership"
    );
    assert!(
        extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &owner_subject,
            "github-actions",
            Permission::View,
        )
        .await
        .expect("owner github-actions permission check"),
        "github actions alerts policy must include deployment owners"
    );
    assert!(
        extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &admin_subject,
            "github-actions",
            Permission::Read,
        )
        .await
        .expect("admin github-actions permission check"),
        "github actions alerts policy must include deployment admins"
    );
    assert!(
        extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &subject,
            "announcements",
            Permission::View,
        )
        .await
        .expect("announcements view permission check"),
        "default announcement route reads should inherit deployment membership"
    );
    assert!(
        extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &owner_subject,
            "chat",
            Permission::View,
        )
        .await
        .expect("owner chat permission check"),
        "default room membership policy must include deployment owners"
    );
    assert!(
        extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &owner_subject,
            "announcements",
            Permission::SendMessage,
        )
        .await
        .expect("owner announcements send permission check"),
        "deployment owners should be allowed to publish announcement extension state"
    );
    assert!(
        !extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &subject,
            "announcements",
            Permission::SendMessage,
        )
        .await
        .expect("announcements send permission check"),
        "announcement extension publishes still require owner permissions"
    );
    state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "announcements"),
                Relation::new("writer"),
                subject.clone(),
            ),
        })
        .await
        .expect("announcement writer tuple");
    assert!(
        !extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &subject,
            "announcements",
            Permission::SendMessage,
        )
        .await
        .expect("announcements writer permission check"),
        "announcement channel writer grants must not bypass server-owner write policy"
    );
    assert!(
        !extension_commands::pubsub::managed_channel_permission_allowed(
            &state,
            &subject,
            "random",
            Permission::SendMessage,
        )
        .await
        .expect("random permission check"),
        "non-default channels still require channel permissions"
    );
}

#[test]
fn test_xmpp_config_prefers_dedicated_database_urls() {
    let _guard = env_lock();
    for key in [
        "WADDLE_XMPP_MAM_DATABASE_URL",
        "WADDLE_XMPP_INBOX_DATABASE_URL",
        "WADDLE_DATABASE_URL",
    ] {
        std::env::remove_var(key);
    }

    std::env::set_var("WADDLE_DATABASE_URL", "postgres://main/runtime");
    std::env::set_var("WADDLE_XMPP_MAM_DATABASE_URL", "postgres://mam/runtime");
    std::env::set_var("WADDLE_XMPP_INBOX_DATABASE_URL", "postgres://inbox/runtime");

    let config = XmppConfig::from_env();
    assert_eq!(
        config.mam_database_url.as_deref(),
        Some("postgres://mam/runtime")
    );
    assert_eq!(
        config.inbox_database_url.as_deref(),
        Some("postgres://inbox/runtime")
    );

    for key in [
        "WADDLE_XMPP_MAM_DATABASE_URL",
        "WADDLE_XMPP_INBOX_DATABASE_URL",
        "WADDLE_DATABASE_URL",
    ] {
        std::env::remove_var(key);
    }
}

#[test]
fn test_xmpp_config_falls_back_to_main_database_url() {
    let _guard = env_lock();
    for key in [
        "WADDLE_XMPP_MAM_DATABASE_URL",
        "WADDLE_XMPP_INBOX_DATABASE_URL",
        "WADDLE_DATABASE_URL",
    ] {
        std::env::remove_var(key);
    }

    std::env::set_var("WADDLE_DATABASE_URL", "postgres://main/runtime");

    let config = XmppConfig::from_env();
    assert_eq!(
        config.mam_database_url.as_deref(),
        Some("postgres://main/runtime")
    );
    assert_eq!(
        config.inbox_database_url.as_deref(),
        Some("postgres://main/runtime")
    );

    std::env::remove_var("WADDLE_DATABASE_URL");
}

#[test]
fn push_service_durability_guard_rejects_sqlite_memory_urls() {
    let runtime = |database_url: &str| crate::config::DatabaseRuntimeConfig {
        driver: crate::db::DatabaseDriver::Sqlite,
        database_url: database_url.to_string(),
    };

    for database_url in [
        "sqlite::memory:",
        "sqlite::memory:?cache=shared",
        "sqlite://:memory:",
        "sqlite:///:memory:",
        "sqlite://file::memory:?cache=shared",
        "sqlite://?mode=memory",
        "sqlite://?mode=memory&cache=private",
        ":memory:",
    ] {
        assert!(
            !http::push_service_database_is_restart_durable(&runtime(database_url)),
            "expected {database_url} to be rejected for durable push publish jobs"
        );
    }
    assert!(http::push_service_database_is_restart_durable(&runtime(
        "sqlite:///tmp/waddle-push.sqlite3?mode=rwc"
    )));
    assert!(http::push_service_database_is_restart_durable(
        &crate::config::DatabaseRuntimeConfig {
            driver: crate::db::DatabaseDriver::Postgres,
            database_url: "postgres://postgres:postgres@localhost/waddle".to_string(),
        }
    ));
}

#[tokio::test]
async fn test_health_endpoint() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Parse response body
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["status"], "healthy");
    assert_eq!(json["service"], "waddle-server");
}

#[tokio::test]
async fn test_healthz_alias_endpoint() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_detailed_health_endpoint() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Parse response body
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["status"], "healthy");
    assert_eq!(json["database"]["status"], "healthy");
    assert!(json["database"]["global_healthy"].as_bool().unwrap());
}

#[tokio::test]
async fn test_ready_endpoint() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ready");
    assert_eq!(json["database"], "ready");
}

#[tokio::test]
async fn test_readyz_alias_endpoint() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_metrics_endpoint() {
    let app = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok()),
        Some("text/plain; version=0.0.4; charset=utf-8")
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let metrics = String::from_utf8(body.to_vec()).unwrap();
    assert!(metrics.contains("waddle_connected_users"));
    assert!(metrics.contains("waddle_messages_per_second"));
    assert!(metrics.contains("waddle_room_count"));
}

#[tokio::test]
async fn test_explicit_cors_allows_credentials() {
    let app = Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .layer(health::build_cors(Some(
            "https://waddle.chat,http://localhost:4321",
        )));

    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/health")
                .header(header::ORIGIN, "https://waddle.chat")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_success());
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("https://waddle.chat")
    );
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-credentials")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
}

#[tokio::test]
async fn test_database_in_app_state() {
    let state = create_test_state().await;

    // Verify we can access the database through AppState
    let health = state.db_pool.health_check().await.unwrap();
    assert!(health.is_healthy());

    let db = state.db_pool.global();

    let runner = MigrationRunner::waddle();
    runner.run(db).await.unwrap();

    // Verify tables exist - use persistent connection for in-memory database
    let conn = db.guard().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='channels'",
            (),
        )
        .await
        .unwrap();

    assert!(rows.next().await.unwrap().is_some());
}

#[tokio::test]
async fn test_seed_fixed_test_account_creates_user() {
    let state = create_test_state().await;
    let password = format!("fixed-account-{}", rand::random::<u64>());
    let config = fixed_account::FixedTestAccountConfig {
        username: "admin".to_string(),
        password: password.clone(),
        domain: "localhost".to_string(),
        email: Some("admin@localhost".to_string()),
    };

    fixed_account::seed_fixed_test_account(&state.db_pool, &config)
        .await
        .unwrap();

    let native_user_store = NativeUserStore::new(state.db_pool.global_actor().clone());
    assert!(native_user_store
        .user_exists(&config.username, &config.domain)
        .await
        .unwrap());
    assert!(native_user_store
        .verify_password(&config.username, &config.domain, &config.password)
        .await
        .unwrap());
}

#[tokio::test]
async fn test_seed_fixed_test_account_replaces_existing_credentials() {
    let state = create_test_state().await;
    let native_user_store = NativeUserStore::new(state.db_pool.global_actor().clone());
    let old_password = format!("fixed-account-old-{}", rand::random::<u64>());
    let new_password = format!("fixed-account-new-{}", rand::random::<u64>());

    native_user_store
        .register(RegisterRequest {
            username: "admin".to_string(),
            domain: "localhost".to_string(),
            password: old_password.clone(),
            email: None,
        })
        .await
        .unwrap();

    let config = fixed_account::FixedTestAccountConfig {
        username: "admin".to_string(),
        password: new_password.clone(),
        domain: "localhost".to_string(),
        email: None,
    };
    fixed_account::seed_fixed_test_account(&state.db_pool, &config)
        .await
        .unwrap();

    assert!(native_user_store
        .verify_password(&config.username, &config.domain, &config.password)
        .await
        .unwrap());
    assert!(!native_user_store
        .verify_password(&config.username, &config.domain, &old_password)
        .await
        .unwrap());

    let credentials = native_user_store
        .get_scram_credentials(&config.username, &config.domain)
        .await
        .unwrap()
        .expect("credentials should exist");
    let salt = BASE64_STANDARD.decode(credentials.salt_b64).unwrap();
    let (stored_key, _) = waddle_xmpp::auth::scram::generate_scram_keys(
        &config.password,
        &salt,
        credentials.iterations,
    );
    assert_eq!(credentials.stored_key, stored_key);
}

async fn test_app() -> Router {
    let state = create_test_state().await;
    let server_config = ServerConfig::test_homeserver();
    let mam_storage = http::create_websocket_mam_storage(None).await.unwrap();
    http::create_router(
        state,
        server_config,
        test_xmpp_config(),
        mam_storage,
        None,
        tokio_util::sync::CancellationToken::new(),
        std::sync::Arc::new(tokio::sync::Notify::new()),
    )
    .await
    .unwrap()
}
