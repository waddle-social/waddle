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
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// XmppConfig for unit tests: uses in-memory SQLite for all storage backends.
fn test_xmpp_config() -> XmppConfig {
    XmppConfig {
        pubsub_database_url: super::config::ResolvedXmppDatabaseUrl::Resolved(
            "sqlite::memory:".to_string(),
        ),
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

    let config = XmppConfig::from_env().expect("config parses");
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

    let config = XmppConfig::from_env().expect("config parses");
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

/// Reset the env vars this test suite mutates between cases. Keeps each
/// `from_env` call observation-independent of every other.
fn reset_xmpp_config_env() {
    for key in [
        "WADDLE_XMPP_DOMAIN",
        "WADDLE_SPACES_JID",
        "WADDLE_MUC_DOMAIN",
        "WADDLE_XMPP_PUBLIC_WEBSOCKET_URL",
    ] {
        std::env::remove_var(key);
    }
}

#[test]
fn test_xmpp_config_parses_trimmed_public_websocket_url() {
    let _guard = env_lock();
    reset_xmpp_config_env();

    std::env::set_var(
        "WADDLE_XMPP_PUBLIC_WEBSOCKET_URL",
        "  wss://xmpp.example.com/ws  ",
    );

    let config = XmppConfig::from_env().expect("public WebSocket URL parses");
    assert_eq!(
        config.public_websocket_url.as_str(),
        "wss://xmpp.example.com/ws"
    );

    reset_xmpp_config_env();
}

#[test]
fn test_xmpp_config_rejects_non_websocket_public_url() {
    let _guard = env_lock();
    reset_xmpp_config_env();

    std::env::set_var(
        "WADDLE_XMPP_PUBLIC_WEBSOCKET_URL",
        "https://xmpp.example.com/ws",
    );

    let error = XmppConfig::from_env().expect_err("HTTP URL must fail startup");
    assert!(
        error.to_string().contains("must use the ws or wss scheme"),
        "unexpected error: {error:#}"
    );

    reset_xmpp_config_env();
}

#[test]
fn test_xmpp_config_rejects_malformed_public_websocket_url() {
    let _guard = env_lock();
    reset_xmpp_config_env();

    std::env::set_var("WADDLE_XMPP_PUBLIC_WEBSOCKET_URL", "not a URL");

    let error = XmppConfig::from_env().expect_err("malformed URL must fail startup");
    assert!(
        error.to_string().contains("is not a valid absolute URL"),
        "unexpected error: {error:#}"
    );
    assert!(!error.to_string().contains("not a URL"));

    reset_xmpp_config_env();
}

#[test]
fn test_xmpp_config_rejects_sensitive_or_ambiguous_websocket_urls() {
    let _guard = env_lock();
    for (configured, expected) in [
        ("wss://alice:secret@xmpp.example.com/ws", "user information"),
        ("wss://xmpp.example.com/ws?token=secret", "query"),
        ("wss://xmpp.example.com/ws#fragment", "fragment"),
    ] {
        reset_xmpp_config_env();
        std::env::set_var("WADDLE_XMPP_PUBLIC_WEBSOCKET_URL", configured);

        let error = XmppConfig::from_env().expect_err("unsafe URL must fail startup");
        assert!(
            error.to_string().contains(expected),
            "unexpected error for {configured}: {error:#}"
        );
        assert!(
            !error.to_string().contains(configured),
            "configuration diagnostics must redact the rejected URL"
        );
        assert!(!error.to_string().contains("secret"));
    }

    reset_xmpp_config_env();
}

#[test]
fn test_xmpp_config_defaults_spaces_and_muc_from_xmpp_domain() {
    let _guard = env_lock();
    reset_xmpp_config_env();

    std::env::set_var("WADDLE_XMPP_DOMAIN", "waddle.example");

    let config = XmppConfig::from_env().expect("config parses");
    assert_eq!(config.spaces_jid.to_string(), "spaces.waddle.example");
    assert_eq!(config.muc_domain.as_str(), "muc.waddle.example");
    assert_eq!(
        config.public_websocket_url.as_str(),
        "ws://waddle.example/ws"
    );

    reset_xmpp_config_env();
}

#[test]
fn test_xmpp_config_env_overrides_spaces_and_muc() {
    let _guard = env_lock();
    reset_xmpp_config_env();

    std::env::set_var("WADDLE_XMPP_DOMAIN", "waddle.example");
    std::env::set_var("WADDLE_SPACES_JID", "communities.waddle.example");
    std::env::set_var("WADDLE_MUC_DOMAIN", "rooms.waddle.example");

    let config = XmppConfig::from_env().expect("config parses");
    assert_eq!(config.spaces_jid.to_string(), "communities.waddle.example");
    assert_eq!(config.muc_domain.as_str(), "rooms.waddle.example");

    reset_xmpp_config_env();
}

#[test]
fn test_xmpp_config_rejects_invalid_spaces_jid() {
    let _guard = env_lock();
    reset_xmpp_config_env();

    // `@<domain>` with an empty nodepart violates `BareJid`'s
    // grammar (`Error::NodeEmpty`).
    std::env::set_var("WADDLE_SPACES_JID", "@example.com");

    let error = XmppConfig::from_env().expect_err("invalid spaces jid must fail-fast");
    let chain = format!("{error:?}");
    assert!(
        chain.contains("WADDLE_SPACES_JID"),
        "error chain should name the offending env var: {chain}"
    );

    reset_xmpp_config_env();
}

#[test]
fn test_xmpp_config_rejects_invalid_muc_domain() {
    let _guard = env_lock();
    reset_xmpp_config_env();

    // An empty string violates `DomainPart::from_str`
    // (`Error::DomainEmpty`); the trim in `parse_optional_env_value`
    // returns `Ok(None)` only when the trimmed result is empty, so we
    // need an explicitly invalid non-empty value. A 2000-byte string
    // exceeds the 1023-byte nameprep limit.
    std::env::set_var("WADDLE_MUC_DOMAIN", "a".repeat(2000));

    let error = XmppConfig::from_env().expect_err("invalid muc domain must fail-fast");
    let chain = format!("{error:?}");
    assert!(
        chain.contains("WADDLE_MUC_DOMAIN"),
        "error chain should name the offending env var: {chain}"
    );

    reset_xmpp_config_env();
}

#[test]
fn push_service_durability_guard_rejects_sqlite_memory_urls() {
    let runtime = |database_url: &str| crate::config::DatabaseRuntimeConfig {
        driver: crate::db::DatabaseDriver::Sqlite,
        database_url: database_url.to_string(),
        ..Default::default()
    };

    for database_url in [
        "sqlite::memory:",
        "sqlite::memory:?cache=shared",
        "sqlite://{memory}:",
        "sqlite:///{memory}:",
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
            ..Default::default()
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
async fn http_request_metrics_use_route_template_and_status_class() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let app = test_app().await;
    let raw_path = "/api/files/nonexistent/private-filename.txt";

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(raw_path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        metrics.histogram_count(
            "http.server.request.duration",
            &[
                ("route", "/api/files/{slot_id}/{filename}"),
                ("status_class", "4xx"),
            ],
        ),
        Some(1),
    );
    assert_eq!(
        metrics.histogram_count(
            "http.server.request.duration",
            &[("route", raw_path), ("status_class", "4xx")],
        ),
        Some(0),
        "raw request paths must never become metric labels",
    );
    assert_eq!(
        metrics.metric_unit("http.server.request.duration"),
        Some("s".to_string()),
    );

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

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        metrics.histogram_count(
            "http.server.request.duration",
            &[("route", "/health"), ("status_class", "2xx")],
        ),
        Some(1),
        "CORS preflights must be observed like every other matched request",
    );
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

// ADR-0017 Phase 3 Slice 2: a node that has self-fenced its clustering
// entity-ownership claims must report not-ready, distinct from (and
// checked before) the database-health branch — a self-fenced node's DB
// pool may otherwise look perfectly healthy.
#[tokio::test]
async fn test_ready_endpoint_reports_not_ready_when_clustering_self_fenced() {
    let state = create_test_state().await;
    // Clone the lifecycle before `state` moves into the router so this test
    // drives the same running-node authority that the production self-fence
    // loop changes. `create_router` finishes startup by arming critical
    // registry supervision and transitioning `Starting` to `Serving`.
    let readiness_handle = state.node_lifecycle.clone();
    let app = test_app_for_state(state).await;

    // A running clustered node loses claim authority: readiness and admission
    // must become non-serving on the already-constructed router.
    readiness_handle.begin_fenced_recovery();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "not_ready");
    assert_eq!(json["admission"], "FencedRecovering");

    // FIX 7(a): recovery — once a node re-registers and clears its
    // self-fenced state, `/ready` must report healthy again on the same
    // router/state, not just flip one direction and stay stuck.
    readiness_handle.serve();
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
}

#[tokio::test]
async fn test_router_startup_preserves_a_concurrent_clustering_self_fence() {
    let state = create_test_state().await;
    let lifecycle = state.node_lifecycle.clone();
    lifecycle.begin_fenced_recovery();

    let app = test_app_for_state(state).await;

    let ready_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ready_response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let ws_response = app
        .clone()
        .oneshot(Request::builder().uri("/ws").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(ws_response.status(), StatusCode::SERVICE_UNAVAILABLE);

    lifecycle.serve();
    let ready_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ready_response.status(), StatusCode::OK);

    let ws_response = app
        .oneshot(
            Request::builder()
                .uri("/ws")
                .header("connection", "upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ws_response.status(), StatusCode::UPGRADE_REQUIRED);
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
    // #1330 contract phase: /metrics is the scrape-liveness stub; every
    // family lives on the OTel meters and answers via the Mimir aliases.
    assert!(metrics.contains("waddle_scrape_ok 1"));
    assert!(!metrics.contains("waddle_connected_users"));
    assert!(!metrics.contains("waddle_messages_per_second"));
    assert!(!metrics.contains("waddle_room_count"));
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
                .header(
                    header::ACCESS_CONTROL_REQUEST_HEADERS,
                    "x-waddle-session-id",
                )
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
    let allow_headers = response
        .headers()
        .get("access-control-allow-headers")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(
        allow_headers
            .split(',')
            .any(|header| header.trim() == "x-waddle-session-id"),
        "explicit CORS allow headers must include x-waddle-session-id: {allow_headers}",
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
    test_app_for_state(state).await
}

async fn test_app_for_state(state: Arc<AppState>) -> Router {
    let server_config = ServerConfig::test_homeserver();
    let mam_storage =
        http::create_websocket_mam_storage(None, false, false, state.db_pool.global())
            .await
            .unwrap();
    let pubsub_database_storage = Arc::new(
        crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("test pubsub storage"),
    );
    http::create_router(http::RouterDeps {
        state,
        server_config,
        xmpp_config: test_xmpp_config(),
        mam_storage,
        pubsub_database_storage,
        acme_http01_challenge_service: None,
        shutdown_handle: waddle_ecdysis::GracefulShutdown::new(std::time::Duration::from_secs(1))
            .handle(),
        drain_complete: std::sync::Arc::new(tokio::sync::Notify::new()),
    })
    .await
    .unwrap()
}
