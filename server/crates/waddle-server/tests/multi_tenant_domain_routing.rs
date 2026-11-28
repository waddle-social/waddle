//! Integration tests for host-derived multi-tenant domain routing, cross-tenant
//! replay isolation, per-tenant OpenFGA authorization routing, and
//! `/etc/waddle/tenants` hot-reload reconciliation.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Extension;
use axum::http::{header, Request, StatusCode};
use axum::routing::get;
use axum::{middleware, Json, Router};
use http_body_util::BodyExt;
use jid::{BareJid, DomainPart};
use kameo::actor::Spawn;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use url::Url;
use waddle_broker::authz::openfga::InProcessOpenFgaServer;
use waddle_broker_core::identity::TenantId;
use waddle_server::auth::identity::{IdentityClaims, IdentityService};
use waddle_server::auth::redirect::TrustedWebOrigins;
use waddle_server::auth::{
    AuthProviderConfig, AuthProviderKind, AuthProviderTokenEndpointAuthMethod,
};
use waddle_server::config::{
    AuthConfig, AuthzBackendSelection, BrokerRuntimeConfig, ClusteringConfig, IngressConfig,
    LinkPreviewConfig, MaxWebSocketConnections, MultiTenantConfig, OpenFgaConfig,
    OpenFgaConsistencySetting, ServerConfig, ServerMode, TursoRuntimeConfig,
};
use waddle_server::db::actor::DbActor;
use waddle_server::db::{Database, MigrationRunner};
use waddle_server::permissions::host_grant::{
    IssuancePolicy, PrincipalPolicy, RestoreEpoch, ServedAccountDomains,
};
use waddle_server::permissions::{
    AttachTenantResolver, CheckPermission, ForTenant, Object, ObjectType, Permission,
    PermissionActor, PermissionError, Relation, Subject, Tuple, WriteTuple,
};
use waddle_server::server::tenancy_bootstrap::{
    bootstrap_turso_tenant_registry, load_tenant_manifests_with_digest, TenantManifestSpec,
};
use waddle_server::server::tenant_routing::{
    reconcile_tenants_dir_once, resolve_tenant_host, ResolvedTenant, TenantHostResolver,
    TenantsReloadOutcome,
};
use waddle_xmpp::protocol::frame::{parse_frame, InboundFrame, ParseError};
use waddle_xmpp::protocol::KeepaliveConfig;
use waddle_xmpp::xep::xep0421::OccupantIdSecret;

fn default_fallback_tenant() -> ResolvedTenant {
    ResolvedTenant::from_static_defaults(
        TenantId::parse("waddle.social").expect("valid tenant id"),
        "https://next.apps.waddle.chat",
        &Url::parse("wss://next.apps.waddle.chat/ws").expect("valid ws url"),
        DomainPart::from_str("waddle.social").expect("valid domain"),
        DomainPart::from_str("muc.waddle.social").expect("valid muc domain"),
        BareJid::from_str("spaces.waddle.social").expect("valid spaces jid"),
        vec!["admin".to_owned()],
    )
    .expect("fallback tenant")
}

fn manifest(tenant_id: &str, hostnames: &[&str], owners: &[&str]) -> TenantManifestSpec {
    TenantManifestSpec {
        tenant_id: tenant_id.to_owned(),
        openfga_store_id: None,
        openfga_model_id: None,
        owners: owners.iter().map(|s| (*s).to_owned()).collect(),
        placement_mode: None,
        hostnames: hostnames.iter().map(|s| (*s).to_owned()).collect(),
        auth_providers: Vec::new(),
    }
}

fn make_openfga_server_config(
    endpoint: &str,
    turso_dir: std::path::PathBuf,
    tenants_dir: Option<std::path::PathBuf>,
) -> ServerConfig {
    ServerConfig {
        mode: ServerMode::HomeServer,
        base_url: "https://next.apps.waddle.chat".to_owned(),
        session_key: "test-session-key-32-bytes-minimum".to_owned(),
        auth: AuthConfig::default(),
        extensions: waddle_extensions::ExtensionConfig::default(),
        link_preview: LinkPreviewConfig::default(),
        ws_keepalive: KeepaliveConfig::default(),
        max_websocket_connections: MaxWebSocketConnections::default(),
        spicedb: None,
        openfga: Some(OpenFgaConfig {
            endpoint: endpoint.to_owned(),
            api_token: None,
            consistency: OpenFgaConsistencySetting::HigherConsistency,
            store_map: HashMap::new(),
        }),
        authz_backend: AuthzBackendSelection::Auto,
        broker: BrokerRuntimeConfig::default(),
        turso: Some(TursoRuntimeConfig {
            data_dir: turso_dir,
            reserved_fds: 64,
            rlimit_nofile: Some(1024),
        }),
        tenancy: MultiTenantConfig {
            default_tenant_id: Some(TenantId::parse("waddle.social").expect("tenant")),
            tenants_dir,
        },
        occupant_id_secret: OccupantIdSecret::try_new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("occupant secret"),
        ingress: IngressConfig::default(),
        clustering: ClusteringConfig::default(),
        apns: None,
        fcm: None,
        trusted_web_origins: TrustedWebOrigins::try_new("https://next.apps.waddle.chat", None)
            .expect("trusted origins"),
        host_grant_restore_epoch: RestoreEpoch::INITIAL,
    }
}

#[tokio::test]
async fn strict_host_routing_splits_operational_and_tenant_routes_and_derives_per_host_urls() {
    let resolver = Arc::new(
        TenantHostResolver::multi_tenant(
            default_fallback_tenant(),
            &[
                manifest(
                    "waddle.social",
                    &["waddle.social", "next.apps.waddle.chat"],
                    &["admin"],
                ),
                manifest("example.waddle.social", &[], &["alice"]),
                manifest("waddle.rawkode.chat", &["waddle.rawkode.chat"], &["bob"]),
            ],
        )
        .expect("multi-tenant resolver"),
    );

    // Mount operational endpoints outside the host-routing middleware, and
    // application routes behind `resolve_tenant_host` (matching `http.rs`).
    let app_routes = Router::new()
        .route(
            "/tenant-info",
            get(|Extension(tenant): Extension<ResolvedTenant>| async move {
                Json(json!({
                    "tenant_id": tenant.tenant_id.as_str(),
                    "canonical_host": tenant.canonical_host,
                    "base_url": tenant.base_url_str(),
                    "ws_url": tenant.public_ws_url.as_str(),
                    "callback_url": tenant.callback_url(),
                    "xmpp_domain": tenant.xmpp_domain.as_str(),
                    "muc_domain": tenant.muc_domain.as_str(),
                    "spaces_jid": tenant.spaces_jid.to_string(),
                    "owns_alice_example": tenant.owns_jid_str("alice@example.waddle.social"),
                    "owns_bob_byo": tenant.owns_jid_str("bob@waddle.rawkode.chat"),
                }))
            }),
        )
        .layer(middleware::from_fn_with_state(
            Arc::clone(&resolver),
            resolve_tenant_host,
        ));
    let router = Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/readyz", get(|| async { StatusCode::OK }))
        .route("/metrics", get(|| async { StatusCode::OK }))
        .merge(app_routes);

    // 1. Operational endpoints succeed with pod-IP Host header.
    for path in ["/healthz", "/readyz", "/metrics"] {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::HOST, "10.42.0.5:3000")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "operational route {path} must accept pod-IP Host"
        );
    }

    // 2. App routes reject unknown host, IP literal, non-standard port with 421,
    //    and strictly ignore X-Forwarded-Host.
    let misdirected_cases = [
        ("evil.example.com", None),
        ("evil.example.com", Some("example.waddle.social")),
        ("example.waddle.social:8443", None),
        ("10.42.0.5:3000", None),
        ("[::1]:443", None),
    ];
    for (host, xfh) in misdirected_cases {
        let mut builder = Request::builder()
            .uri("/tenant-info")
            .header(header::HOST, host);
        if let Some(xfh_val) = xfh {
            builder = builder.header("x-forwarded-host", xfh_val);
        }
        let resp = router
            .clone()
            .oneshot(builder.body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(
            resp.status(),
            StatusCode::MISDIRECTED_REQUEST,
            "host={host} xfh={xfh:?} must fail closed with 421"
        );
    }

    // 3. Duplicate Host header and missing Host header return 400 Bad Request.
    let dup_resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/tenant-info")
                .header(header::HOST, "example.waddle.social")
                .header(header::HOST, "waddle.rawkode.chat")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(dup_resp.status(), StatusCode::BAD_REQUEST);

    let missing_resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/tenant-info")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(missing_resp.status(), StatusCode::BAD_REQUEST);

    // 4. Valid tenant hosts (including uppercase + standard :443 port) resolve
    //    their exact tenant identity, per-host URLs, and JID ownership.
    for (host_header, expected_tenant, expected_base, expected_ws, owns_alice, owns_bob) in [
        (
            "next.apps.waddle.chat",
            "waddle.social",
            "https://next.apps.waddle.chat",
            "wss://next.apps.waddle.chat/ws",
            false,
            false,
        ),
        (
            "EXAMPLE.WADDLE.SOCIAL:443",
            "example.waddle.social",
            "https://example.waddle.social",
            "wss://example.waddle.social/ws",
            true,
            false,
        ),
        (
            "waddle.rawkode.chat",
            "waddle.rawkode.chat",
            "https://waddle.rawkode.chat",
            "wss://waddle.rawkode.chat/ws",
            false,
            true,
        ),
    ] {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/tenant-info")
                    .header(header::HOST, host_header)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.expect("body").to_bytes();
        let info: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(info["tenant_id"].as_str(), Some(expected_tenant));
        assert_eq!(info["xmpp_domain"].as_str(), Some(expected_tenant));
        assert_eq!(info["base_url"].as_str(), Some(expected_base));
        assert_eq!(info["ws_url"].as_str(), Some(expected_ws));
        assert_eq!(
            info["callback_url"].as_str(),
            Some(format!("{expected_base}/api/auth/callback").as_str())
        );
        assert_eq!(
            info["muc_domain"].as_str(),
            Some(format!("muc.{expected_tenant}").as_str())
        );
        assert_eq!(
            info["spaces_jid"].as_str(),
            Some(format!("spaces.{expected_tenant}").as_str())
        );
        assert_eq!(info["owns_alice_example"].as_bool(), Some(owns_alice));
        assert_eq!(info["owns_bob_byo"].as_bool(), Some(owns_bob));
    }

    // 5. Dynamic CORS origin validation and domain->base_url resolution.
    assert!(resolver.is_allowed_origin_str("https://example.waddle.social"));
    assert!(resolver.is_allowed_origin_str("https://waddle.rawkode.chat"));
    assert!(resolver.is_allowed_origin_str("https://next.apps.waddle.chat"));
    assert!(!resolver.is_allowed_origin_str("https://evil.example.com"));
    assert_eq!(
        resolver
            .base_url_for_domain("example.waddle.social")
            .as_deref(),
        Some("https://example.waddle.social")
    );
    assert_eq!(
        resolver
            .base_url_for_domain("waddle.rawkode.chat")
            .as_deref(),
        Some("https://waddle.rawkode.chat")
    );

    // 6. Per-tenant TrustedWebOrigins extends return-URL allowlist cleanly.
    let base_trusted =
        TrustedWebOrigins::try_new("https://next.apps.waddle.chat", None).expect("trusted");
    let example_trusted = base_trusted
        .with_tenant_base(&Url::parse("https://example.waddle.social").expect("example base url"));
    assert!(example_trusted
        .resolve_return_url(Some("https://example.waddle.social/chat"))
        .is_ok());
    assert!(example_trusted
        .resolve_return_url(Some("https://evil.example.com/steal"))
        .is_err());
}

#[tokio::test]
async fn permission_actor_for_tenant_isolates_openfga_stores_per_tenant() {
    let openfga = InProcessOpenFgaServer::start()
        .await
        .expect("start in-process OpenFGA");

    let config = make_openfga_server_config(openfga.endpoint(), "/tmp".into(), None);

    let (actor_instance, broker_backend) =
        PermissionActor::from_server_config_with_broker_backend(&config, "waddle.social", &[])
            .await
            .expect("boot OpenFGA permission actor");
    let broker_backend = broker_backend.expect("broker backend");

    let tenant_example = TenantId::parse("example.waddle.social").expect("tenant example");
    let tenant_byo = TenantId::parse("waddle.rawkode.chat").expect("tenant byo");
    let tenant_default = TenantId::parse("waddle.social").expect("tenant default");

    // Provision distinct OpenFGA stores for each tenant.
    broker_backend
        .provision_tenant_store(&tenant_example)
        .await
        .expect("provision example store");
    broker_backend
        .provision_tenant_store(&tenant_byo)
        .await
        .expect("provision byo store");

    let perm_actor = PermissionActor::spawn(actor_instance);
    let subject = Subject::user("alice@example.waddle.social");
    let object = Object::new(ObjectType::Server, "deployment");

    perm_actor
        .ask(ForTenant::new(
            tenant_example.clone(),
            WriteTuple {
                tuple: Tuple::new(object.clone(), Relation::new("owner"), subject.clone()),
            },
        ))
        .await
        .expect("write owner tuple for example tenant");

    let allowed_example = perm_actor
        .ask(ForTenant::new(
            tenant_example,
            CheckPermission {
                subject: subject.clone(),
                permission: Permission::Owner,
                object: object.clone(),
            },
        ))
        .await
        .expect("check example tenant");
    assert!(allowed_example.allowed);

    let allowed_byo = perm_actor
        .ask(ForTenant::new(
            tenant_byo,
            CheckPermission {
                subject: subject.clone(),
                permission: Permission::Owner,
                object: object.clone(),
            },
        ))
        .await
        .expect("check byo tenant");
    assert!(
        !allowed_byo.allowed,
        "owner tuple in example.waddle.social must not leak to waddle.rawkode.chat"
    );

    let allowed_default = perm_actor
        .ask(ForTenant::new(
            tenant_default,
            CheckPermission {
                subject,
                permission: Permission::Owner,
                object,
            },
        ))
        .await
        .expect("check default tenant");
    assert!(
        !allowed_default.allowed,
        "owner tuple in example.waddle.social must not leak to waddle.social"
    );
}

#[tokio::test]
async fn tenants_dir_hot_reload_provisions_before_swap_rejects_conflicts_and_evicts_without_purge()
{
    let temp = TempDir::new().expect("tempdir");
    let tenants_dir = temp.path().join("tenants");
    let turso_dir = temp.path().join("turso");
    std::fs::create_dir_all(&tenants_dir).expect("create tenants dir");
    std::fs::create_dir_all(&turso_dir).expect("create turso dir");

    let openfga = InProcessOpenFgaServer::start()
        .await
        .expect("start openfga");
    let server_cfg = make_openfga_server_config(
        openfga.endpoint(),
        turso_dir.clone(),
        Some(tenants_dir.clone()),
    );
    let (_, broker_backend) =
        PermissionActor::from_server_config_with_broker_backend(&server_cfg, "waddle.social", &[])
            .await
            .expect("openfga backend");
    let broker_backend = broker_backend.expect("broker backend");

    // Seed only waddle.social initially.
    std::fs::write(
        tenants_dir.join("waddle.social.json"),
        json!({
            "tenantId": "waddle.social",
            "hostnames": ["waddle.social", "next.apps.waddle.chat"],
            "owners": ["admin"]
        })
        .to_string(),
    )
    .expect("write initial manifest");

    let (initial_digest, initial_manifests) = load_tenant_manifests_with_digest(&tenants_dir)
        .await
        .expect("load initial manifests")
        .expect("manifests present");
    let mut last_digest = Some(initial_digest);
    let default_tid = TenantId::parse("waddle.social").expect("default tid");
    let registry = bootstrap_turso_tenant_registry(
        &TursoRuntimeConfig {
            data_dir: turso_dir.clone(),
            reserved_fds: 64,
            rlimit_nofile: Some(1024),
        },
        &default_tid,
        &initial_manifests,
        Some(Arc::clone(&broker_backend)),
    )
    .await
    .expect("bootstrap turso registry");
    let resolver = Arc::new(
        TenantHostResolver::multi_tenant(default_fallback_tenant(), &initial_manifests)
            .expect("resolver"),
    );

    // Initially, example.waddle.social is unknown.
    assert!(resolver
        .snapshot()
        .expect("snapshot")
        .get_by_host("example.waddle.social")
        .is_none());

    // Add two new tenants (`example.waddle.social` and `waddle.rawkode.chat`).
    let example_store = broker_backend
        .provision_tenant_store(&TenantId::parse("example.waddle.social").expect("tid"))
        .await
        .expect("provision example store");
    let byo_store = broker_backend
        .provision_tenant_store(&TenantId::parse("waddle.rawkode.chat").expect("tid"))
        .await
        .expect("provision byo store");

    std::fs::write(
        tenants_dir.join("example.waddle.social.json"),
        json!({
            "tenantId": "example.waddle.social",
            "openfgaStoreId": example_store.store_id,
            "openfgaModelId": example_store.model_version,
            "owners": ["alice"]
        })
        .to_string(),
    )
    .expect("write example manifest");
    std::fs::write(
        tenants_dir.join("waddle.rawkode.chat.json"),
        json!({
            "tenantId": "waddle.rawkode.chat",
            "hostnames": ["waddle.rawkode.chat"],
            "openfgaStoreId": byo_store.store_id,
            "openfgaModelId": byo_store.model_version,
            "owners": ["bob"]
        })
        .to_string(),
    )
    .expect("write byo manifest");

    let changed = reconcile_tenants_dir_once(
        &tenants_dir,
        &mut last_digest,
        &resolver,
        Some(&registry),
        Some(&broker_backend),
    )
    .await
    .expect("reconcile add");
    assert!(
        matches!(changed, TenantsReloadOutcome::Reloaded { .. }),
        "digest changed so reconcile must swap table, got {changed:?}"
    );
    assert!(resolver
        .snapshot()
        .expect("snapshot")
        .get_by_host("example.waddle.social")
        .is_some());
    assert!(resolver
        .snapshot()
        .expect("snapshot")
        .get_by_host("waddle.rawkode.chat")
        .is_some());
    assert!(
        turso_dir.join("example.waddle.social.db").exists(),
        "Turso DB must be provisioned before swap"
    );
    assert!(
        turso_dir.join("waddle.rawkode.chat.db").exists(),
        "Turso DB must be provisioned before swap"
    );

    // No-op when directory is unchanged.
    let unchanged = reconcile_tenants_dir_once(
        &tenants_dir,
        &mut last_digest,
        &resolver,
        Some(&registry),
        Some(&broker_backend),
    )
    .await
    .expect("reconcile noop");
    assert_eq!(unchanged, TenantsReloadOutcome::Unchanged);

    // Duplicate hostname conflict fails safe and retains the previous table!
    std::fs::write(
        tenants_dir.join("conflict.waddle.social.json"),
        json!({
            "tenantId": "conflict.waddle.social",
            "hostnames": ["example.waddle.social"]
        })
        .to_string(),
    )
    .expect("write conflicting manifest");
    let conflict_err = reconcile_tenants_dir_once(
        &tenants_dir,
        &mut last_digest,
        &resolver,
        Some(&registry),
        Some(&broker_backend),
    )
    .await;
    assert!(
        conflict_err.is_err(),
        "duplicate hostname across tenants must fail safe"
    );
    assert_eq!(
        resolver
            .snapshot()
            .expect("snapshot")
            .get_by_host("example.waddle.social")
            .expect("prior table retained")
            .tenant_id
            .as_str(),
        "example.waddle.social"
    );

    // Remove the conflicting file and remove `waddle.rawkode.chat.json`:
    // routing drops `waddle.rawkode.chat`, evicts it from registry, and
    // preserves `waddle.rawkode.chat.db` on disk (never purges!).
    std::fs::remove_file(tenants_dir.join("conflict.waddle.social.json")).expect("rm conflict");
    std::fs::remove_file(tenants_dir.join("waddle.rawkode.chat.json")).expect("rm byo");
    let removed = reconcile_tenants_dir_once(
        &tenants_dir,
        &mut last_digest,
        &resolver,
        Some(&registry),
        Some(&broker_backend),
    )
    .await
    .expect("reconcile remove");
    assert!(matches!(removed, TenantsReloadOutcome::Reloaded { .. }));
    assert!(resolver
        .snapshot()
        .expect("snapshot")
        .get_by_host("waddle.rawkode.chat")
        .is_none());
    assert!(
        turso_dir.join("waddle.rawkode.chat.db").exists(),
        "hot-reload removal must evict only and NEVER purge tenant DB on disk"
    );
}

#[test]
fn xmpp_open_to_attribute_parses_domain_and_rejects_node_qualified_values() {
    let ok = parse_frame(
        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.waddle.social" from="alice@example.waddle.social" version="1.0"/>"#,
    )
    .expect("valid open");
    match ok {
        InboundFrame::Open { from, to } => {
            assert_eq!(
                from,
                Some(BareJid::from_str("alice@example.waddle.social").expect("bare jid"))
            );
            assert_eq!(
                to,
                Some(DomainPart::from_str("example.waddle.social").expect("domain"))
            );
        }
        other => panic!("expected InboundFrame::Open, got {other:?}"),
    }

    let bad_node = parse_frame(
        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="alice@example.waddle.social" version="1.0"/>"#,
    );
    assert!(matches!(bad_node, Err(ParseError::InvalidOpenTo)));

    let bad_upper = parse_frame(
        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="Example.Waddle.Social" version="1.0"/>"#,
    );
    assert!(matches!(bad_upper, Err(ParseError::InvalidOpenTo)));
}

#[tokio::test]
async fn same_oidc_identity_can_sign_into_multiple_tenants_on_sqlite() {
    let db = Database::in_memory("multi-tenant-identity")
        .await
        .expect("in-memory sqlite");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("run migrations");
    let db_actor = DbActor::spawn(DbActor::new(db));
    let identity_service = IdentityService::new(db_actor);

    let provider = AuthProviderConfig {
        id: "github".to_owned(),
        display_name: "GitHub".to_owned(),
        kind: AuthProviderKind::Oidc,
        dynamic_client_registration: false,
        client_id: "client".to_owned(),
        client_secret: String::new(),
        token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::NoAuthentication,
        require_dpop: false,
        scopes: vec!["openid".to_owned()],
        issuer: Some("https://idp.example.com".to_owned()),
        authorization_endpoint: None,
        token_endpoint: None,
        userinfo_endpoint: None,
        jwks_uri: None,
        subject_claim: "sub".to_owned(),
        username_claim: None,
        email_claim: None,
    };
    let claims = IdentityClaims {
        subject: "user-42".to_owned(),
        issuer: Some("https://idp.example.com".to_owned()),
        preferred_username: Some("rawkode".to_owned()),
        name: Some("Rawkode".to_owned()),
        email: Some("rawkode@example.com".to_owned()),
        email_verified: Some(true),
        avatar_url: None,
        raw_claims: json!({"sub": "user-42", "preferred_username": "rawkode"}),
    };

    let first = identity_service
        .resolve_or_create_user(&provider, &claims, "waddle.social")
        .await
        .expect("sign in on waddle.social");
    let second = identity_service
        .resolve_or_create_user(&provider, &claims, "example.waddle.social")
        .await
        .expect("same OIDC identity signs in on example.waddle.social");
    let third = identity_service
        .resolve_or_create_user(&provider, &claims, "waddle.rawkode.chat")
        .await
        .expect("same OIDC identity signs in on waddle.rawkode.chat");

    assert_eq!(first.user.jid, "rawkode@waddle.social");
    assert_eq!(second.user.jid, "rawkode@example.waddle.social");
    assert_eq!(third.user.jid, "rawkode@waddle.rawkode.chat");

    // Repeat sign-in on each tenant reconciles the exact same tenant account.
    let second_again = identity_service
        .resolve_or_create_user(&provider, &claims, "example.waddle.social")
        .await
        .expect("repeat sign in on example.waddle.social");
    assert_eq!(second_again.user.jid, "rawkode@example.waddle.social");
}

#[tokio::test]
async fn permission_actor_with_tenant_resolver_routes_bare_ops_admits_tenant_privileges_and_is_idempotent(
) {
    let openfga = InProcessOpenFgaServer::start()
        .await
        .expect("start in-process OpenFGA");
    let config = make_openfga_server_config(openfga.endpoint(), "/tmp".into(), None);

    let (actor_instance, broker_backend) =
        PermissionActor::from_server_config_with_broker_backend(&config, "waddle.social", &[])
            .await
            .expect("boot OpenFGA permission actor");
    let broker_backend = broker_backend.expect("broker backend");

    let tenant_example = TenantId::parse("example.waddle.social").expect("tenant example");
    let tenant_byo = TenantId::parse("waddle.rawkode.chat").expect("tenant byo");
    broker_backend
        .provision_tenant_store(&tenant_example)
        .await
        .expect("provision example store");
    broker_backend
        .provision_tenant_store(&tenant_byo)
        .await
        .expect("provision byo store");

    let resolver = Arc::new(
        TenantHostResolver::multi_tenant(
            default_fallback_tenant(),
            &[
                manifest("waddle.social", &["next.apps.waddle.chat"], &["admin"]),
                manifest("example.waddle.social", &[], &["alice"]),
                manifest("waddle.rawkode.chat", &["waddle.rawkode.chat"], &["bob"]),
            ],
        )
        .expect("resolver"),
    );

    // Build privileged issuance restricted to the default domain (`waddle.social`),
    // mirroring `lifecycle.rs::replica_privileged_issuance`.
    let default_domain =
        waddle_xmpp::identity::CanonicalDomain::parse_canonical("waddle.social").expect("domain");
    let principals = PrincipalPolicy::new(ServedAccountDomains::new([default_domain]), None);
    let policy = IssuancePolicy::for_principals(
        &principals,
        [
            waddle_xmpp::identity::CanonicalDomain::parse_canonical("muc.waddle.social")
                .expect("muc"),
            waddle_xmpp::identity::CanonicalDomain::parse_canonical("spaces.waddle.social")
                .expect("spaces"),
        ],
    )
    .expect("issuance policy");

    let perm_actor = PermissionActor::spawn(actor_instance.with_privileged_issuance(policy));
    perm_actor
        .ask(AttachTenantResolver(Arc::clone(&resolver)))
        .await
        .expect("attach resolver");

    // 1. Privileged Channel owner write for a non-default tenant succeeds via bare WriteTuple
    //    AND routes automatically to example.waddle.social's OpenFGA store!
    let alice = Subject::user("alice@example.waddle.social");
    let channel = Object::new(ObjectType::Channel, "general@muc.example.waddle.social");
    let owner_tuple = Tuple::new(channel.clone(), Relation::new("owner"), alice.clone());
    perm_actor
        .ask(WriteTuple {
            tuple: owner_tuple.clone(),
        })
        .await
        .expect("first privileged write for non-default tenant");

    // 2. Duplicate WriteTuple (repeat / second-device login) succeeds idempotently!
    perm_actor
        .ask(WriteTuple { tuple: owner_tuple })
        .await
        .expect("duplicate OpenFGA write must succeed idempotently");

    // 3. Bare CheckPermission routes to example.waddle.social's store (allowed),
    //    while explicit check on waddle.rawkode.chat is denied (isolated store!).
    let allowed = perm_actor
        .ask(CheckPermission {
            subject: alice.clone(),
            permission: Permission::Owner,
            object: channel.clone(),
        })
        .await
        .expect("bare check");
    assert!(allowed.allowed);

    let byo_allowed = perm_actor
        .ask(ForTenant::new(
            tenant_byo,
            CheckPermission {
                subject: alice,
                permission: Permission::Owner,
                object: channel.clone(),
            },
        ))
        .await
        .expect("byo check");
    assert!(
        !byo_allowed.allowed,
        "tuple written via bare WriteTuple must land in example.waddle.social store, not waddle.rawkode.chat"
    );

    // 4. Untrusted external domain is STILL rejected with UntrustedPrivilegedSubject!
    let mallory = Subject::user("mallory@evil.example");
    let err = perm_actor
        .ask(WriteTuple {
            tuple: Tuple::new(channel, Relation::new("owner"), mallory),
        })
        .await
        .expect_err("untrusted domain must be refused");
    assert!(matches!(
        err,
        kameo::error::SendError::HandlerError(PermissionError::UntrustedPrivilegedSubject)
    ));
}
