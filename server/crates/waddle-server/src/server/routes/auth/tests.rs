use super::{
    router, Arc, AuthProviderTokenEndpointAuthMethod, AuthState, BrowserSessionTransport,
    PendingAuthorization, PendingFlow, Session, Utc,
};
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
    let db_pool = DatabasePool::try_new(config, pool_config).await.unwrap();
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
            tenant_id: auth_state.tenant_resolver.fallback_tenant().tenant_id,
            provider_id: "missing-provider".to_string(),
            issuer: None,
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

async fn insert_pending_flow(auth_state: &AuthState, state: &str, flow: PendingFlow) {
    auth_state
        .auth_handshake
        .insert_pending(&PendingAuthorization {
            state: state.to_string(),
            tenant_id: auth_state.tenant_resolver.fallback_tenant().tenant_id,
            provider_id: "missing-provider".to_string(),
            issuer: None,
            nonce: "nonce".to_string(),
            code_verifier: "verifier".to_string(),
            redirect_uri: "http://localhost:3000/api/auth/callback".to_string(),
            client_id: "client-id".to_string(),
            client_secret: String::new(),
            token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::NoAuthentication,
            require_dpop: false,
            flow,
            created_at: Utc::now(),
        })
        .await
        .expect("insert pending authorization");
}

async fn callback_error(auth_state: Arc<AuthState>, state: &str) -> (StatusCode, String) {
    let response = router(auth_state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/auth/callback?state={state}&code=authorization-code"
                ))
                .body(Body::empty())
                .expect("callback request"),
        )
        .await
        .expect("callback response");
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// A browser sign-in whose return URL is on another site must be refused
/// before the provider exchange: the fragment transport would otherwise put
/// the new session id in that site's URL.
#[tokio::test]
async fn callback_refuses_browser_return_to_untrusted_origin() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, _) = create_test_auth_state(&server_config).await;
    for (state, next) in [
        ("fragment-offsite", "https://attacker.example/"),
        ("fragment-backslash", "/\\attacker.example/"),
        (
            "fragment-userinfo",
            "http://localhost:3000@attacker.example/",
        ),
    ] {
        insert_pending_flow(
            &auth_state,
            state,
            PendingFlow::Browser {
                next: Some(next.to_string()),
                session_transport: BrowserSessionTransport::Fragment,
            },
        )
        .await;

        let (status, body) = callback_error(auth_state.clone(), state).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{next}");
        assert!(body.contains("sign-in return URL"), "{next}: {body}");
        assert!(
            !body.contains("invalid_client"),
            "{next} reached the provider"
        );
    }
}

#[tokio::test]
async fn callback_refuses_xmpp_flow_without_safe_redirect_or_pkce() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, _) = create_test_auth_state(&server_config).await;
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    for (state, redirect, code_challenge) in [
        (
            "xmpp-offsite",
            "https://attacker.example/cb",
            Some(challenge.to_string()),
        ),
        ("xmpp-no-pkce", "http://127.0.0.1:9000/cb", None),
    ] {
        insert_pending_flow(
            &auth_state,
            state,
            PendingFlow::Xmpp {
                client_redirect_uri: redirect.to_string(),
                client_state: None,
                client_code_challenge: code_challenge,
                scopes: vec![waddle_xmpp::xep::xep0493::OAuthScope::ClientNormal],
            },
        )
        .await;

        let (status, body) = callback_error(auth_state.clone(), state).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{state}");
        assert!(
            !body.contains("invalid_client"),
            "{state} reached the provider: {body}"
        );
    }
}

#[tokio::test]
async fn xmpp_authorize_refuses_unsafe_redirects_and_missing_pkce() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, _) = create_test_auth_state(&server_config).await;
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    for query in [
        format!(
            "redirect_uri=https%3A%2F%2Fattacker.example%2Fcb&code_challenge={challenge}&code_challenge_method=S256"
        ),
        "redirect_uri=http%3A%2F%2F127.0.0.1%3A9000%2Fcb".to_string(),
        format!("redirect_uri=http%3A%2F%2F127.0.0.1%3A9000%2Fcb&code_challenge={challenge}"),
        format!(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A9000%2Fcb&code_challenge={challenge}&code_challenge_method=plain"
        ),
    ] {
        let response = crate::server::routes::xmpp_oauth::router(auth_state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/auth/xmpp/authorize?{query}"))
                    .body(Body::empty())
                    .expect("authorize request"),
            )
            .await
            .expect("authorize response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "invalid_request", "{query}");
    }
}

/// The session id is a bearer credential: whoever holds it is the user. No
/// span on the session lookup, validation, or logout path may record it.
#[tokio::test(flavor = "current_thread")]
async fn session_id_never_becomes_a_span_field() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, _actor) = create_test_auth_state(&server_config).await;
    let session = Session::new("alice@example.com", "alice", "alice");
    auth_state
        .session_manager
        .create_session(&session)
        .await
        .unwrap();
    let spans = test_support::acquire_spans();

    let response = router(auth_state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/auth/session?session_id={}", session.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    auth_state
        .session_manager
        .validate_session(&session.id)
        .await
        .unwrap();
    let response = router(auth_state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "session_id": session.id }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    assert_eq!(
        spans.recorded_field_containing(&session.id),
        None,
        "a span recorded the bearer session id"
    );
}

/// A failed lookup is logged and returned to the caller, so the error must
/// not carry the presented credential either.
#[tokio::test]
async fn unknown_session_error_does_not_carry_the_presented_id() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, _actor) = create_test_auth_state(&server_config).await;

    let error = auth_state
        .session_manager
        .validate_session("presented-bearer-credential")
        .await
        .expect_err("unknown session");

    assert!(!format!("{error} {error:?}").contains("presented-bearer-credential"));
}

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
// base64url(SHA-256(PKCE_VERIFIER)), RFC 7636 Appendix B.
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).expect("JSON body")
}

/// XEP-0493 § OAuth Scopes: the server MUST support `xmpp:client:normal`,
/// and it is the scope the metadata names.
#[tokio::test]
async fn xmpp_oauth_metadata_advertises_client_normal() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, _) = create_test_auth_state(&server_config).await;
    let response = crate::server::routes::xmpp_oauth::router(auth_state)
        .oneshot(
            Request::builder()
                .uri("/.well-known/oauth-authorization-server")
                .body(Body::empty())
                .expect("metadata request"),
        )
        .await
        .expect("metadata response");
    assert_eq!(response.status(), StatusCode::OK);
    let json = json_body(response).await;
    assert_eq!(
        json["scopes_supported"],
        serde_json::json!(["xmpp:client:normal"])
    );
    assert_eq!(
        json["code_challenge_methods_supported"],
        serde_json::json!(["S256"])
    );
}

/// RFC 6749 §4.1.2.1: with a valid redirect URI an unsupported scope is
/// reported by redirecting to it with `error=invalid_scope` and the state.
#[tokio::test]
async fn xmpp_authorize_redirects_an_unsupported_scope_to_the_client() {
    let server_config = ServerConfig::test_homeserver();
    let (auth_state, _) = create_test_auth_state(&server_config).await;
    let response = crate::server::routes::xmpp_oauth::router(auth_state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/auth/xmpp/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A9000%2Fcb\
                     &code_challenge={PKCE_CHALLENGE}&code_challenge_method=S256\
                     &scope=xmpp%3Aclient%3Anormal%20admin&state=abc"
                ))
                .body(Body::empty())
                .expect("authorize request"),
        )
        .await
        .expect("authorize response");
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "http://127.0.0.1:9000/cb?error=invalid_scope&state=abc"
    );
}

/// The token response reports the granted scope, and the exchange records a
/// grant the owner can list and revoke.
#[tokio::test]
async fn xmpp_token_exchange_reports_the_scope_and_records_a_grant() {
    use crate::auth::oauth_grants::OAuthGrantStore;
    use waddle_xmpp::xep::xep0493::OAuthScope;

    let server_config = ServerConfig::test_homeserver();
    let (auth_state, _) = create_test_auth_state(&server_config).await;
    let session = Session::new("alice@example.com", "alice", "alice");
    auth_state
        .session_manager
        .create_session(&session)
        .await
        .unwrap();
    auth_state
        .auth_handshake
        .insert_xmpp_code(
            "code-1",
            &super::XmppAuthCode {
                session_id: session.id.clone(),
                redirect_uri: "http://127.0.0.1:9000/cb".to_string(),
                code_challenge: Some(PKCE_CHALLENGE.to_string()),
                scopes: vec![OAuthScope::ClientNormal],
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();

    let response = crate::server::routes::xmpp_oauth::router(auth_state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/xmpp/token")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "grant_type=authorization_code&code=code-1\
                     &redirect_uri=http%3A%2F%2F127.0.0.1%3A9000%2Fcb&code_verifier={PKCE_VERIFIER}"
                )))
                .expect("token request"),
        )
        .await
        .expect("token response");
    assert_eq!(response.status(), StatusCode::OK);
    let json = json_body(response).await;
    assert_eq!(json["scope"], "xmpp:client:normal");
    assert_eq!(json["token_type"], "Bearer");
    assert_eq!(json["access_token"], session.id);

    let grants = OAuthGrantStore::new(&auth_state.session_manager)
        .list("alice@example.com")
        .await
        .unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].scopes, vec![OAuthScope::ClientNormal]);
}
