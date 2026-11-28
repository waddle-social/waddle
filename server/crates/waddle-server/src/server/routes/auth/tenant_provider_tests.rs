//! Per-tenant sign-in providers: each tenant only sees and uses the
//! providers its manifest declares, and a handshake started on one tenant
//! can never be finished on another.

use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::{middleware, Router};
use chrono::Duration;
use http_body_util::BodyExt;
use jid::{BareJid, DomainPart};
use serde_json::Value;
use tower::ServiceExt;
use waddle_broker_core::identity::TenantId;

use super::{
    router, AuthProviderTokenEndpointAuthMethod, AuthState, BrowserSessionTransport,
    DeviceAuthStatus, DeviceAuthorization, PendingAuthorization, PendingFlow, Utc,
};
use crate::auth::{AuthProviderKind, TenantAuthProviderSpec};
use crate::config::ServerConfig;
use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
use crate::server::tenancy_bootstrap::TenantManifestSpec;
use crate::server::tenant_routing::{
    resolve_tenant_host, ResolvedTenant, TenantHostResolver, TenantRoutingError,
};
use crate::server::AppState;

const RKA: &str = "rka.example";
const COLONY: &str = "colony.example";
const SILENT: &str = "silent.example";

fn provider(id: &str, issuer: &str) -> TenantAuthProviderSpec {
    TenantAuthProviderSpec {
        id: id.to_owned(),
        display_name: id.to_owned(),
        kind: AuthProviderKind::Oidc,
        dynamic_client_registration: false,
        client_id: format!("{id}-client"),
        require_dpop: false,
        scopes: vec!["openid".to_owned(), "profile".to_owned()],
        issuer: issuer.to_owned(),
        authorization_endpoint: None,
        token_endpoint: None,
        userinfo_endpoint: None,
        jwks_uri: None,
        subject_claim: "sub".to_owned(),
        username_claim: Some("preferred_username".to_owned()),
        email_claim: None,
    }
}

fn manifest(tenant_id: &str, providers: Vec<TenantAuthProviderSpec>) -> TenantManifestSpec {
    TenantManifestSpec {
        tenant_id: tenant_id.to_owned(),
        openfga_store_id: None,
        openfga_model_id: None,
        owners: Vec::new(),
        placement_mode: None,
        hostnames: vec![tenant_id.to_owned()],
        auth_providers: providers,
    }
}

fn manifests() -> Vec<TenantManifestSpec> {
    vec![
        manifest(RKA, vec![provider("rka", "https://id.rka.example")]),
        manifest(
            COLONY,
            vec![provider("colony", "https://colony.example.test")],
        ),
        manifest(SILENT, Vec::new()),
    ]
}

fn fallback_tenant() -> ResolvedTenant {
    ResolvedTenant::from_static_defaults(
        TenantId::parse(RKA).expect("tenant id"),
        "https://rka.example",
        &url::Url::parse("wss://rka.example/ws").expect("ws url"),
        DomainPart::from_str(RKA).expect("domain"),
        DomainPart::from_str("muc.rka.example").expect("muc domain"),
        BareJid::from_str("spaces.rka.example").expect("spaces jid"),
        Vec::new(),
    )
    .expect("fallback tenant")
}

async fn multi_tenant_app() -> (Router, Arc<AuthState>, Arc<TenantHostResolver>) {
    let db_pool = DatabasePool::try_new(DatabaseConfig::default(), PoolConfig)
        .await
        .expect("db pool");
    MigrationRunner::global()
        .run(db_pool.global())
        .await
        .expect("migrations");
    let resolver = Arc::new(
        TenantHostResolver::multi_tenant(fallback_tenant(), &manifests()).expect("resolver"),
    );
    let mut app_state = AppState::new(Arc::new(db_pool));
    app_state.tenant_resolver = Arc::clone(&resolver);

    let mut server_config = ServerConfig::test_homeserver();
    // A server-wide provider must never leak into a multi-tenant tenant.
    server_config.auth.providers = vec![provider("server-wide", "https://global.example").into()];
    let auth_state = Arc::new(AuthState::new(
        Arc::new(app_state),
        &server_config,
        &url::Url::parse("wss://rka.example/ws").expect("ws url"),
        None,
    ));
    let app = router(Arc::clone(&auth_state))
        .merge(crate::server::routes::device::router(Arc::clone(
            &auth_state,
        )))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&resolver),
            resolve_tenant_host,
        ));
    (app, auth_state, resolver)
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn get(host: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::HOST, host)
        .body(Body::empty())
        .expect("request")
}

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).expect("tenant id")
}

fn pending(state: &str, tenant_id: &str, provider_id: &str, issuer: &str) -> PendingAuthorization {
    PendingAuthorization {
        state: state.to_owned(),
        tenant_id: tenant(tenant_id),
        provider_id: provider_id.to_owned(),
        issuer: Some(issuer.to_owned()),
        nonce: "nonce".to_owned(),
        code_verifier: "verifier".to_owned(),
        redirect_uri: format!("https://{tenant_id}/api/auth/callback"),
        client_id: format!("{provider_id}-client"),
        client_secret: String::new(),
        token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::NoAuthentication,
        require_dpop: false,
        flow: PendingFlow::Browser {
            next: None,
            session_transport: BrowserSessionTransport::Cookie,
        },
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn each_tenant_lists_only_its_manifest_providers() {
    let (app, _, _) = multi_tenant_app().await;
    for (host, expected) in [
        (RKA, vec!["rka"]),
        (COLONY, vec!["colony"]),
        (SILENT, Vec::new()),
    ] {
        let (status, body) = send(&app, get(host, "/api/auth/providers")).await;
        assert_eq!(status, StatusCode::OK, "{host}");
        let ids: Vec<&str> = body
            .as_array()
            .expect("provider list")
            .iter()
            .map(|p| p["id"].as_str().expect("provider id"))
            .collect();
        assert_eq!(ids, expected, "providers listed on {host}");
    }
}

#[tokio::test]
async fn start_refuses_a_provider_declared_by_another_tenant() {
    let (app, _, _) = multi_tenant_app().await;
    for (host, foreign) in [(RKA, "colony"), (COLONY, "rka"), (SILENT, "server-wide")] {
        let (status, _) = send(
            &app,
            get(host, &format!("/api/auth/start?provider={foreign}")),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{host} must refuse provider {foreign}"
        );
    }
}

#[tokio::test]
async fn callback_refuses_a_sign_in_started_on_another_tenant() {
    let (app, auth_state, _) = multi_tenant_app().await;
    auth_state
        .auth_handshake
        .insert_pending(&pending(
            "cross-tenant",
            RKA,
            "rka",
            "https://id.rka.example",
        ))
        .await
        .expect("insert pending");

    let (status, body) = send(
        &app,
        get(COLONY, "/api/auth/callback?state=cross-tenant&code=c"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_state", "{body}");
}

#[tokio::test]
async fn callback_refuses_a_provider_whose_issuer_changed_since_start() {
    let (app, auth_state, _) = multi_tenant_app().await;
    auth_state
        .auth_handshake
        .insert_pending(&pending(
            "stale-issuer",
            RKA,
            "rka",
            "https://previous-issuer.example",
        ))
        .await
        .expect("insert pending");

    let (status, body) = send(
        &app,
        get(RKA, "/api/auth/callback?state=stale-issuer&code=c"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_provider", "{body}");
}

#[tokio::test]
async fn device_code_cannot_be_verified_on_another_tenant() {
    let (app, auth_state, _) = multi_tenant_app().await;
    let device = DeviceAuthorization {
        device_code: "device-rka".to_owned(),
        user_code: "RKA-CODE".to_owned(),
        tenant_id: tenant(RKA),
        provider_id: "rka".to_owned(),
        expires_at: Utc::now() + Duration::minutes(10),
        status: DeviceAuthStatus::Pending,
        session_id: None,
    };
    auth_state
        .auth_handshake
        .insert_device(&device)
        .await
        .expect("insert device");

    let request = Request::builder()
        .method("POST")
        .uri("/api/auth/device/verify")
        .header(header::HOST, COLONY)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("user_code=RKA-CODE"))
        .expect("request");
    let (status, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_user_code", "{body}");

    let untouched = auth_state
        .auth_handshake
        .get_device("device-rka")
        .await
        .expect("get device")
        .expect("device still present");
    assert_eq!(untouched.status, DeviceAuthStatus::Pending);
}

#[test]
fn invalid_manifest_providers_are_rejected_by_the_routing_table() {
    let resolver =
        TenantHostResolver::multi_tenant(fallback_tenant(), &manifests()).expect("resolver");
    let mut broken = provider("rka", "https://id.rka.example");
    broken.scopes = vec!["profile".to_owned()];
    let mut updated = manifests();
    updated[0].auth_providers = vec![broken];

    let err = resolver
        .build_table(&updated)
        .expect_err("an OIDC provider without the openid scope is invalid");
    assert!(
        matches!(err, TenantRoutingError::InvalidAuthProviders { ref tenant_id, .. } if tenant_id == RKA),
        "{err}"
    );
    let table = resolver.snapshot().expect("snapshot");
    assert_eq!(
        table
            .auth_providers(&tenant(RKA))
            .expect("previous providers kept")
            .list()
            .len(),
        1
    );
}

#[test]
fn manifest_providers_parse_from_camel_case_json_and_reject_secrets() {
    let parsed: TenantManifestSpec = serde_json::from_value(serde_json::json!({
        "tenantId": RKA,
        "authProviders": [{
            "id": "rka",
            "displayName": "Rawkode Academy",
            "kind": "oidc",
            "clientId": "waddle-rawkode-chat",
            "scopes": ["openid", "profile", "email"],
            "issuer": "https://id.rawkode.academy",
            "usernameClaim": "preferred_username",
            "emailClaim": "email"
        }]
    }))
    .expect("manifest parses");
    let config: crate::auth::AuthProviderConfig = parsed.auth_providers[0].clone().into();
    assert_eq!(config.client_id, "waddle-rawkode-chat");
    assert_eq!(
        config.token_endpoint_auth_method,
        AuthProviderTokenEndpointAuthMethod::NoAuthentication
    );
    assert!(config.client_secret.is_empty());

    let with_secret = serde_json::from_value::<TenantManifestSpec>(serde_json::json!({
        "tenantId": RKA,
        "authProviders": [{
            "id": "rka",
            "displayName": "Rawkode Academy",
            "kind": "oidc",
            "clientId": "c",
            "clientSecret": "s",
            "scopes": ["openid"],
            "issuer": "https://id.rawkode.academy"
        }]
    }));
    assert!(
        with_secret.is_err(),
        "manifest providers must not carry client secrets"
    );
}
