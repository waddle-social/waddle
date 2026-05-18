use super::*;
use crate::config::ServerConfig;
use crate::db::actor::{DbActor, DbExecute};
use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
use crate::server::AppState;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn create_test_auth_state(
    server_config: &ServerConfig,
) -> (Arc<AuthState>, kameo::actor::ActorRef<DbActor>) {
    let config = DatabaseConfig::default();
    let pool_config = PoolConfig;
    let db_pool = DatabasePool::new(config, pool_config).await.unwrap();
    MigrationRunner::global()
        .run(db_pool.global())
        .await
        .unwrap();
    let actor = db_pool.global_actor().clone();

    let app_state = Arc::new(AppState::new(Arc::new(db_pool)));
    (
        Arc::new(AuthState::new(app_state, server_config, None)),
        actor,
    )
}

#[tokio::test]
async fn session_response_includes_jid_and_websocket_url() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, actor) = create_test_auth_state(&server_config).await;
    let session = Session::new("user-1", "alice", "alice");
    auth_state
        .session_manager
        .create_session(&session)
        .await
        .unwrap();
    actor
        .ask(DbExecute {
            sql: "UPDATE users SET avatar_url = ? WHERE id = ?".to_string(),
            params: vec![
                "https://avatars.example.com/alice.png".into(),
                session.user_id.clone().into(),
            ],
        })
        .await
        .unwrap();

    let app = router(auth_state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/auth/session")
                .header(header::COOKIE, format!("waddle_session={}", session.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let expected_jid = format!("alice@{}", auth_state.xmpp_domain);
    assert_eq!(json["username"], "alice");
    assert_eq!(
        json["avatar_url"].as_str(),
        Some("https://avatars.example.com/alice.png")
    );
    assert_eq!(json["jid"].as_str(), Some(expected_jid.as_str()));
    assert_eq!(
        json["xmpp_websocket_url"].as_str(),
        Some("ws://localhost:3000/ws")
    );
}

#[tokio::test]
async fn session_response_falls_back_to_raw_claim_profile_avatar() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, actor) = create_test_auth_state(&server_config).await;
    let session = Session::new("user-1", "alice", "alice");
    auth_state
        .session_manager
        .create_session(&session)
        .await
        .unwrap();
    actor
        .ask(DbExecute {
            sql: "INSERT INTO auth_identities (id, user_id, provider_id, issuer, subject, email, email_verified, raw_claims_json, created_at, last_login_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            params: vec![
                "identity-1".into(),
                session.user_id.clone().into(),
                "colony".into(),
                "https://colony.waddle.social".into(),
                "subject-1".into(),
                crate::db::Value::from(Option::<String>::None),
                crate::db::Value::from(Option::<bool>::None),
                r#"{"profile":"https://cdn.example.com/avatar.png"}"#.into(),
                "2026-04-15T00:00:00Z".into(),
                "2026-04-15T00:00:00Z".into(),
            ],
        })
        .await
        .unwrap();

    let app = router(auth_state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/auth/session")
                .header(header::COOKIE, format!("waddle_session={}", session.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["avatar_url"].as_str(),
        Some("https://cdn.example.com/avatar.png")
    );
}

#[tokio::test]
async fn secure_cookie_header_tracks_base_url_scheme() {
    let mut secure_config = ServerConfig::test_homeserver();
    secure_config.base_url = "https://server.waddle.social".to_string();
    let (secure_state, _) = create_test_auth_state(&secure_config).await;
    assert!(secure_state
        .session_cookie_header(Some("token"), 60)
        .contains("Secure"));

    let insecure_config = ServerConfig::test_homeserver();
    let (insecure_state, _) = create_test_auth_state(&insecure_config).await;
    assert!(!insecure_state
        .session_cookie_header(Some("token"), 60)
        .contains("Secure"));
}
