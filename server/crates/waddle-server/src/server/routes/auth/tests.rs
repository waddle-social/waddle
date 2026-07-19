use super::*;
use crate::config::ServerConfig;
use crate::db::actor::{DbActor, DbExecute};
use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
use crate::server::AppState;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use waddle_xmpp::telemetry::test_support;

pub(crate) async fn create_test_auth_state(
    server_config: &ServerConfig,
) -> (Arc<AuthState>, kameo::actor::ActorRef<DbActor>) {
    let public_websocket_url =
        url::Url::parse("ws://localhost:3000/ws").expect("test WebSocket URL");
    create_test_auth_state_with_websocket_url(server_config, &public_websocket_url).await
}

async fn create_test_auth_state_with_websocket_url(
    server_config: &ServerConfig,
    public_websocket_url: &url::Url,
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
        Arc::new(AuthState::new(
            app_state,
            server_config,
            public_websocket_url,
            None,
        )),
        actor,
    )
}

#[tokio::test]
async fn session_response_includes_jid_websocket_url_and_link_preview_media_origin() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, actor) = create_test_auth_state(&server_config).await;
    let session = Session::new("alice@example.com", "alice", "alice");
    auth_state
        .session_manager
        .create_session(&session)
        .await
        .unwrap();
    actor
        .ask(DbExecute {
            sql: "UPDATE users SET avatar_url = ? WHERE jid = ?".to_string(),
            params: vec![
                "https://avatars.example.com/alice.png".into(),
                session.user_jid.clone().into(),
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
    assert_eq!(
        json["link_preview_media_origin"].as_str(),
        Some("http://localhost:3000")
    );
}

#[tokio::test]
async fn session_response_advertises_public_wss_when_app_base_url_is_internal_http() {
    let mut server_config = ServerConfig::test_homeserver();
    server_config.base_url = "http://localhost:3000".to_string();
    let public_websocket_url =
        url::Url::parse("wss://xmpp.example.com/ws").expect("public WebSocket URL");
    let (auth_state, actor) =
        create_test_auth_state_with_websocket_url(&server_config, &public_websocket_url).await;
    let session = Session::new("alice@example.com", "alice", "alice");
    auth_state
        .session_manager
        .create_session(&session)
        .await
        .expect("create session");

    let response = router(auth_state)
        .oneshot(
            Request::builder()
                .uri("/api/auth/session")
                .header(header::COOKIE, format!("waddle_session={}", session.id))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("session response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("session JSON");
    assert_eq!(
        json["xmpp_websocket_url"].as_str(),
        Some("wss://xmpp.example.com/ws")
    );

    drop(actor);
}

#[tokio::test]
async fn session_response_falls_back_to_raw_claim_profile_avatar() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, actor) = create_test_auth_state(&server_config).await;
    let session = Session::new("alice@example.com", "alice", "alice");
    auth_state
        .session_manager
        .create_session(&session)
        .await
        .unwrap();
    actor
        .ask(DbExecute {
            sql: "INSERT INTO auth_identities (id, user_jid, provider_id, issuer, subject, email, email_verified, raw_claims_json, created_at, last_login_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            params: vec![
                "identity-1".into(),
                session.user_jid.clone().into(),
                "colony".into(),
                "https://colony.waddle.social".into(),
                "subject-1".into(),
                Option::<String>::None.into(),
                Option::<i64>::None.into(),
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

#[tokio::test]
async fn callback_records_state_success_before_subsequent_provider_failure() {
    let guard = test_support::acquire().await;
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, _) = create_test_auth_state(&server_config).await;
    auth_state
        .auth_handshake
        .insert_pending(&PendingAuthorization {
            state: "validated-state".to_string(),
            provider_id: "missing-provider".to_string(),
            nonce: "nonce".to_string(),
            code_verifier: "verifier".to_string(),
            redirect_uri: "http://localhost:3000/api/auth/callback".to_string(),
            client_id: "client-id".to_string(),
            client_secret: String::new(),
            token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::NoAuthentication,
            require_dpop: false,
            flow: PendingFlow::Browser {
                next: Some("/".to_string()),
                session_transport: BrowserSessionTransport::Cookie,
            },
            created_at: Utc::now(),
        })
        .await
        .expect("insert pending authorization");

    let response = router(auth_state)
        .oneshot(
            Request::builder()
                .uri("/api/auth/callback?state=validated-state&code=authorization-code")
                .body(Body::empty())
                .expect("callback request"),
        )
        .await
        .expect("callback response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        guard.counter_sum("waddle.auth.success", &[("stage", "state")]),
        Some(1),
    );
    assert_eq!(
        guard.counter_sum(
            "waddle.auth.failures",
            &[("stage", "oidc_callback"), ("error_code", "invalid_client")]
        ),
        Some(1),
    );
}
