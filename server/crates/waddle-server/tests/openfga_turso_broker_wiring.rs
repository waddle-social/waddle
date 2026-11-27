//! Integration tests for Phase 2 production startup wiring:
//! - Shared env-var contract (`WADDLE_DB_DRIVER=turso`, `WADDLE_TURSO_*`,
//!   `WADDLE_AUTHZ_BACKEND`, `WADDLE_OPENFGA_*`, `WADDLE_BROKER_*`,
//!   `WADDLE_SCW_KMS_*`, `WADDLE_DEFAULT_TENANT_ID`, `WADDLE_TENANTS_DIR`)
//! - `PermissionActor` booting cleanly on OpenFGA with ZERO SpiceDB config,
//!   including `LookupResources` and `LookupSubjects`
//! - `ServerTursoTenantStore` + `ServerTenantRegistry` + `WADDLE_TENANTS_DIR`
//!   bootstrap with zero Postgres dependency
//! - Production `BrokerExtensionInferenceHost` wiring with Scaleway KMS
//!   (mock HTTP), persistent Turso ledgers (`admission.db`,
//!   `destruction_ledger.db`, `custody.db`), shared `OpenFgaPermissionBackend`,
//!   and `ServerBrokerCredentialResolver` resolving real enrolled credentials!

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::Utc;
use jid::BareJid;
use kameo::actor::Spawn;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use waddle_broker::admission::invocation::{PayerSelection, SignedInferenceInvocation};
use waddle_broker::authz::openfga::InProcessOpenFgaServer;
use waddle_broker::custody::EnrollCredentialInput;
use waddle_broker::secret::SecretBytes;
use waddle_broker_core::admission::{BudgetLimits, RoomScopeKey};
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{AccountId, SpiffeId, TenantId};
use waddle_broker_core::placement::FenceGeneration;
use waddle_broker_core::store::{TenantQueryScope, TenantStore, TenantStoreError};
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::{PluginId, WaddleId};
use waddle_server::auth::redirect::TrustedWebOrigins;
use waddle_server::config::{
    AuthConfig, AuthzBackendKind, AuthzBackendSelection, BrokerKmsProviderConfig,
    BrokerRuntimeConfig, ClusteringConfig, DatabaseRuntimeConfig, IngressConfig, LinkPreviewConfig,
    MaxWebSocketConnections, MultiTenantConfig, OpenFgaConfig, OpenFgaConsistencySetting,
    ServerConfig, ServerMode, TursoRuntimeConfig,
};
use waddle_server::db::actor::{DbActor, DbExecute};
use waddle_server::db::{Database, DatabaseDriver, MigrationRunner};
use waddle_server::permissions::host_grant::RestoreEpoch;
use waddle_server::permissions::{
    CheckPermission, LookupResources, LookupSubjects, Object, ObjectType, Permission,
    PermissionActor, Relation, Subject, SubjectType, Tuple, WriteTuple,
};
use waddle_server::server::extension_inference_host::{
    build_broker_runtime_from_config, BrokerCredentialResolver, NoopRoomDisclosureEmitter,
};
use waddle_server::server::tenancy_bootstrap::{
    bootstrap_turso_tenant_registry, extract_openfga_store_refs, load_tenant_manifests,
};
use waddle_tenancy::data_ops::{TenantDataOps, TenantUserRecord};
use waddle_tenancy::turso_store::{TursoPoolConfig, TursoTenantStore};
use waddle_xmpp::protocol::KeepaliveConfig;
use waddle_xmpp::xep::xep0421::OccupantIdSecret;

struct MockScwKey {
    id: String,
    project_id: String,
    name: String,
    state: String,
    material: [u8; 32],
}

#[derive(Default)]
struct MockScwState {
    expected_token: String,
    keys: HashMap<String, MockScwKey>,
    create_count: usize,
    encrypt_count: usize,
    decrypt_count: usize,
}

struct MockScwServer {
    base_url: String,
    state: Arc<RwLock<MockScwState>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl MockScwServer {
    async fn start(expected_token: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock scw");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(RwLock::new(MockScwState {
            expected_token: expected_token.to_owned(),
            ..MockScwState::default()
        }));
        let st = Arc::clone(&state);
        let (tx, mut rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else {
                            break;
                        };
                        let st_conn = Arc::clone(&st);
                        tokio::spawn(async move {
                            let _ = handle_scw_conn(&mut stream, st_conn).await;
                        });
                    }
                }
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            state,
            shutdown: Some(tx),
        }
    }
}

impl Drop for MockScwServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

async fn handle_scw_conn(
    stream: &mut tokio::net::TcpStream,
    state: Arc<RwLock<MockScwState>>,
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    let header_str = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = header_str.lines();
    let req_line = lines.next().unwrap_or_default();
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let full_path = parts.next().unwrap_or_default().to_owned();

    let mut content_length = 0usize;
    let mut auth_token = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            } else if k.eq_ignore_ascii_case("x-auth-token") {
                auth_token = Some(v.trim().to_owned());
            }
        }
    }
    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body_json: Option<Value> = if content_length > 0 && buf.len() >= body_start + content_length
    {
        serde_json::from_slice(&buf[body_start..body_start + content_length]).ok()
    } else {
        None
    };

    let (status, reason, resp_body) = route_scw(&state, &method, &full_path, auth_token, body_json);
    let resp_str = resp_body.map(|v| v.to_string()).unwrap_or_default();
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        resp_str.len(),
        resp_str
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await
}

fn route_scw(
    state: &Arc<RwLock<MockScwState>>,
    method: &str,
    full_path: &str,
    auth_token: Option<String>,
    body: Option<Value>,
) -> (u16, &'static str, Option<Value>) {
    let mut guard = state.write().expect("lock");
    if auth_token.as_deref() != Some(guard.expected_token.as_str()) {
        return (401, "Unauthorized", Some(json!({ "message": "denied" })));
    }
    let (path, query) = full_path.split_once('?').unwrap_or((full_path, ""));
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    if segments.len() == 5 && segments[4] == "keys" {
        if method == "GET" {
            let parsed_url = url::Url::parse(&format!("http://localhost/?{query}")).expect("url");
            let params: HashMap<String, String> = parsed_url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let name_filter = params.get("name").map(String::as_str);
            let keys: Vec<Value> = guard
                .keys
                .values()
                .filter(|k| name_filter.is_none_or(|n| k.name == n))
                .map(|k| {
                    json!({
                        "id": k.id,
                        "project_id": k.project_id,
                        "name": k.name,
                        "state": k.state,
                    })
                })
                .collect();
            return (200, "OK", Some(json!({ "keys": keys })));
        }
        if method == "POST" {
            let val = body.expect("create key body");
            let id = Uuid::new_v4().to_string();
            let project_id = val["project_id"].as_str().unwrap_or_default().to_owned();
            let name = val["name"].as_str().unwrap_or_default().to_owned();
            let mut material = [0u8; 32];
            material[..16].copy_from_slice(Uuid::new_v4().as_bytes());
            material[16..].copy_from_slice(Uuid::new_v4().as_bytes());
            guard.create_count += 1;
            guard.keys.insert(
                id.clone(),
                MockScwKey {
                    id: id.clone(),
                    project_id: project_id.clone(),
                    name: name.clone(),
                    state: "enabled".to_owned(),
                    material,
                },
            );
            return (
                200,
                "OK",
                Some(json!({
                    "id": id,
                    "project_id": project_id,
                    "name": name,
                    "state": "enabled",
                })),
            );
        }
    }

    if segments.len() == 7 && segments[4] == "keys" && method == "POST" {
        let key_id = segments[5];
        let action = segments[6];
        let Some(key_material) = guard.keys.get(key_id).map(|k| k.material) else {
            return (404, "Not Found", Some(json!({ "message": "not found" })));
        };
        match action {
            "encrypt" => {
                guard.encrypt_count += 1;
                let val = body.expect("encrypt");
                let pt = STANDARD
                    .decode(val["plaintext"].as_str().unwrap_or_default())
                    .expect("pt");
                let aad = STANDARD
                    .decode(val["associated_data"].as_str().unwrap_or_default())
                    .expect("aad");
                let cipher = Aes256Gcm::new_from_slice(&key_material).expect("aes");
                let nonce = [3u8; 12];
                let ct = cipher
                    .encrypt(
                        &Nonce::from(nonce),
                        Payload {
                            msg: &pt,
                            aad: &aad,
                        },
                    )
                    .expect("enc");
                let mut out = Vec::with_capacity(12 + ct.len());
                out.extend_from_slice(&nonce);
                out.extend_from_slice(&ct);
                return (
                    200,
                    "OK",
                    Some(json!({ "ciphertext": STANDARD.encode(out) })),
                );
            }
            "decrypt" => {
                guard.decrypt_count += 1;
                let val = body.expect("decrypt");
                let raw = STANDARD
                    .decode(val["ciphertext"].as_str().unwrap_or_default())
                    .expect("ct");
                let aad = STANDARD
                    .decode(val["associated_data"].as_str().unwrap_or_default())
                    .expect("aad");
                let nonce: [u8; 12] = raw[..12].try_into().expect("nonce");
                let cipher = Aes256Gcm::new_from_slice(&key_material).expect("aes");
                let pt = cipher
                    .decrypt(
                        &Nonce::from(nonce),
                        Payload {
                            msg: &raw[12..],
                            aad: &aad,
                        },
                    )
                    .expect("dec");
                return (200, "OK", Some(json!({ "plaintext": STANDARD.encode(pt) })));
            }
            "delete-key-material" => {
                if let Some(k) = guard.keys.get_mut(key_id) {
                    k.state = "destroyed".to_owned();
                }
                return (200, "OK", Some(json!({})));
            }
            _ => {}
        }
    }

    (404, "Not Found", Some(json!({ "message": "unknown" })))
}

#[test]
fn env_var_contract_parses_turso_openfga_broker_and_tenancy_without_postgres_or_spicedb() {
    let db_cfg = DatabaseRuntimeConfig::from_vars([
        ("WADDLE_DB_DRIVER", "turso"),
        ("WADDLE_TURSO_DATA_DIR", "/tmp/waddle-tenants"),
        ("WADDLE_TURSO_RESERVED_FDS", "128"),
        ("WADDLE_TURSO_RLIMIT_NOFILE", "4096"),
    ])
    .expect("turso db runtime config");
    assert_eq!(db_cfg.driver, DatabaseDriver::Sqlite);
    assert_eq!(
        db_cfg.database_url,
        "sqlite:///tmp/waddle-tenants/control.db?mode=rwc"
    );
    let turso = db_cfg.turso.expect("turso config enabled");
    assert_eq!(turso.data_dir.to_string_lossy(), "/tmp/waddle-tenants");
    assert_eq!(turso.reserved_fds, 128);
    assert_eq!(turso.rlimit_nofile, Some(4096));

    let openfga = OpenFgaConfig::from_vars([
        ("WADDLE_OPENFGA_ENDPOINT", "http://openfga:8080"),
        ("WADDLE_OPENFGA_API_TOKEN", "secret-fga-token"),
        ("WADDLE_OPENFGA_CONSISTENCY", "higher_consistency"),
        (
            "WADDLE_OPENFGA_STORE_MAP_JSON",
            r#"{"waddle.social":{"store_id":"01STORE","model_id":"01MODEL"}}"#,
        ),
    ])
    .expect("openfga config")
    .expect("openfga present");
    assert_eq!(openfga.endpoint, "http://openfga:8080");
    assert_eq!(
        openfga.consistency,
        OpenFgaConsistencySetting::HigherConsistency
    );
    assert!(!format!("{openfga:?}").contains("secret-fga-token"));
    let default_tid = TenantId::parse("waddle.social").expect("tid");
    assert_eq!(openfga.store_map[&default_tid].store_id, "01STORE");
    assert_eq!(openfga.store_map[&default_tid].model_id, "01MODEL");

    assert_eq!(
        AuthzBackendSelection::from_vars([("WADDLE_AUTHZ_BACKEND", "openfga")]).expect("authz"),
        AuthzBackendSelection::Explicit(AuthzBackendKind::OpenFga)
    );
    assert_eq!(
        AuthzBackendSelection::from_vars(std::iter::empty::<(&str, &str)>()).expect("auto"),
        AuthzBackendSelection::Auto
    );

    let broker = BrokerRuntimeConfig::from_vars([
        ("WADDLE_BROKER_ENABLED", "true"),
        ("WADDLE_BROKER_DATA_DIR", "/var/lib/waddle/broker"),
        ("WADDLE_BROKER_KMS_PROVIDER", "scaleway"),
        ("WADDLE_SCW_KMS_BASE_URL", "https://api.scaleway.com"),
        ("WADDLE_SCW_KMS_REGION", "fr-par"),
        (
            "WADDLE_SCW_KMS_PROJECT_ID",
            "11111111-2222-3333-4444-555555555555",
        ),
        ("WADDLE_SCW_KMS_AUTH_TOKEN", "scw-secret-key"),
        (
            "WADDLE_SCW_KMS_ROOT_KEY_ID",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        ),
        (
            "WADDLE_CELL_SPIFFE_ID",
            "spiffe://waddle.social/ns/waddle-next/sa/waddle-server",
        ),
    ])
    .expect("broker config");
    assert!(broker.enabled);
    assert_eq!(
        broker
            .resolved_admission_db_path()
            .expect("admission db")
            .to_string_lossy(),
        "/var/lib/waddle/broker/admission.db"
    );
    let (custody_db, destruction_db) = broker
        .resolved_custody_ledger_paths()
        .expect("custody paths");
    assert_eq!(
        custody_db.to_string_lossy(),
        "/var/lib/waddle/broker/custody.db"
    );
    assert_eq!(
        destruction_db.to_string_lossy(),
        "/var/lib/waddle/broker/destruction_ledger.db"
    );
    assert!(matches!(broker.kms, BrokerKmsProviderConfig::Scaleway(_)));
    assert!(!format!("{broker:?}").contains("scw-secret-key"));

    let tenancy = MultiTenantConfig::from_vars([
        ("WADDLE_DEFAULT_TENANT_ID", "waddle.social"),
        ("WADDLE_TENANTS_DIR", "/etc/waddle/tenants"),
    ])
    .expect("tenancy config");
    assert_eq!(
        tenancy
            .resolved_default_tenant_id("fallback.example")
            .expect("default tenant"),
        default_tid
    );
    assert_eq!(
        tenancy.tenants_dir.expect("tenants dir").to_string_lossy(),
        "/etc/waddle/tenants"
    );
}

#[tokio::test]
async fn openfga_turso_scaleway_broker_boot_and_credential_resolution_e2e() {
    let dir = TempDir::new().expect("tempdir");
    let tenants_dir = dir.path().join("tenants-manifests");
    let turso_data_dir = dir.path().join("turso-tenants");
    let broker_data_dir = dir.path().join("broker");
    tokio::fs::create_dir_all(&tenants_dir)
        .await
        .expect("create manifests dir");

    // 1. Start in-process OpenFGA HTTP server and mock Scaleway KMS HTTP server (zero SpiceDB, zero Postgres!).
    let fga_server = InProcessOpenFgaServer::start()
        .await
        .expect("start openfga");
    let scw_token = "scw-test-secret-token";
    let scw_server = MockScwServer::start(scw_token).await;

    // Write a WaddleTenant-style JSON manifest into WADDLE_TENANTS_DIR.
    let manifest_json = json!({
        "tenantId": "acme.waddle.social",
        "owners": ["rawkode"],
        "placementMode": "SharedPool"
    });
    tokio::fs::write(
        tenants_dir.join("acme.waddle.social.json"),
        serde_json::to_string_pretty(&manifest_json).expect("json"),
    )
    .await
    .expect("write manifest");

    // 2. Build ServerConfig with OpenFGA auto-selected (spicedb: None) + Turso + Scaleway KMS broker.
    let mut server_config = ServerConfig {
        mode: ServerMode::HomeServer,
        base_url: "http://localhost:3000".to_owned(),
        session_key: "test-session-key-32-bytes-minimum".to_owned(),
        auth: AuthConfig::default(),
        extensions: waddle_extensions::ExtensionConfig::default(),
        link_preview: LinkPreviewConfig::default(),
        ws_keepalive: KeepaliveConfig::default(),
        max_websocket_connections: MaxWebSocketConnections::default(),
        spicedb: None,
        openfga: None,
        authz_backend: AuthzBackendSelection::Auto,
        broker: BrokerRuntimeConfig::default(),
        turso: None,
        tenancy: MultiTenantConfig::default(),
        occupant_id_secret: OccupantIdSecret::try_new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("occupant secret"),
        ingress: IngressConfig::default(),
        clustering: ClusteringConfig::default(),
        apns: None,
        fcm: None,
        trusted_web_origins: TrustedWebOrigins::try_new("http://localhost:3000", None)
            .expect("trusted origins"),
        host_grant_restore_epoch: RestoreEpoch::INITIAL,
    };
    assert!(server_config.spicedb.is_none());
    server_config.openfga = Some(OpenFgaConfig {
        endpoint: fga_server.endpoint().to_owned(),
        api_token: None,
        consistency: OpenFgaConsistencySetting::HigherConsistency,
        store_map: HashMap::new(),
    });
    server_config.authz_backend = AuthzBackendSelection::Auto;
    assert_eq!(
        server_config.resolved_authz_backend(),
        Some(AuthzBackendKind::OpenFga)
    );
    server_config.turso = Some(TursoRuntimeConfig {
        data_dir: turso_data_dir.clone(),
        reserved_fds: 64,
        rlimit_nofile: Some(1024),
    });
    server_config.tenancy = MultiTenantConfig {
        default_tenant_id: Some(TenantId::parse("waddle.social").expect("tenant")),
        tenants_dir: Some(tenants_dir.clone()),
    };
    server_config.broker = BrokerRuntimeConfig::from_vars([
        ("WADDLE_BROKER_ENABLED", "true"),
        (
            "WADDLE_BROKER_DATA_DIR",
            broker_data_dir.to_str().expect("utf8 path"),
        ),
        ("WADDLE_BROKER_KMS_PROVIDER", "scaleway"),
        ("WADDLE_SCW_KMS_BASE_URL", scw_server.base_url.as_str()),
        ("WADDLE_SCW_KMS_REGION", "fr-par"),
        (
            "WADDLE_SCW_KMS_PROJECT_ID",
            "11111111-2222-3333-4444-555555555555",
        ),
        ("WADDLE_SCW_KMS_AUTH_TOKEN", scw_token),
        (
            "WADDLE_CELL_SPIFFE_ID",
            "spiffe://waddle.social/ns/waddle-next/sa/waddle-server",
        ),
    ])
    .expect("broker config");

    // 3. Boot PermissionActor on OpenFGA with ZERO SpiceDB config and verify LookupResources / LookupSubjects!
    let manifests = load_tenant_manifests(&tenants_dir)
        .await
        .expect("load manifests");
    assert_eq!(manifests.len(), 1);
    let extra_refs = extract_openfga_store_refs(&manifests).expect("extract refs");
    let (permission_actor_impl, broker_perm_backend) =
        PermissionActor::from_server_config_with_broker_backend(
            &server_config,
            "waddle.social",
            &extra_refs,
        )
        .await
        .expect("boot PermissionActor on OpenFGA without SpiceDB");
    assert_eq!(permission_actor_impl.backend_name(), "openfga");
    let broker_perm_backend = broker_perm_backend.expect("shared broker OpenFGA backend");
    let permission_actor = PermissionActor::spawn(permission_actor_impl);

    let space = Object::new(ObjectType::Space, "community");
    let channel = Object::new(ObjectType::Channel, "general");
    let alice = Subject::user("alice@waddle.social");
    let bob = Subject::user("bob@waddle.social");

    permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(space.clone(), Relation::new("owner"), alice.clone()),
        })
        .await
        .expect("write space owner");
    permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                channel.clone(),
                Relation::new("parent"),
                Subject {
                    subject_type: SubjectType::Space,
                    id: "community".to_owned(),
                    relation: None,
                },
            ),
        })
        .await
        .expect("write channel parent");
    permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(channel.clone(), Relation::new("member"), bob.clone()),
        })
        .await
        .expect("write channel member");

    let check = permission_actor
        .ask(CheckPermission {
            subject: alice.clone(),
            permission: Permission::Read,
            object: channel.clone(),
        })
        .await
        .expect("check inherited read");
    assert!(check.allowed);

    let resources = permission_actor
        .ask(LookupResources {
            subject: alice.clone(),
            permission: Permission::Read,
            object_type: ObjectType::Channel,
        })
        .await
        .expect("LookupResources via OpenFGA list-objects");
    assert_eq!(resources, vec![channel.clone()]);

    let mut subjects = permission_actor
        .ask(LookupSubjects {
            object: channel.clone(),
            permission: Permission::Read,
            subject_type: SubjectType::User,
        })
        .await
        .expect("LookupSubjects via OpenFGA list-users");
    subjects.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(subjects, vec![alice.clone(), bob.clone()]);

    // 4. Boot ServerTursoTenantStore + ServerTenantRegistry for default + manifest tenants (zero Postgres!).
    let default_tenant = TenantId::parse("waddle.social").expect("default tenant");
    let acme_tenant = TenantId::parse("acme.waddle.social").expect("acme tenant");
    let _registry = bootstrap_turso_tenant_registry(
        server_config.turso.as_ref().expect("turso cfg"),
        &default_tenant,
        &manifests,
        Some(Arc::clone(&broker_perm_backend)),
    )
    .await
    .expect("bootstrap turso tenant registry");
    assert!(turso_data_dir.join("waddle.social.db").exists());
    assert!(turso_data_dir.join("acme.waddle.social.db").exists());

    // 5. Boot production Broker runtime with Scaleway KMS + persistent Turso ledgers + shared OpenFGA!
    let db = Database::in_memory("openfga-turso-broker-wiring")
        .await
        .expect("sqlite control db");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");
    let alice_account = AccountId::new_v4();
    let db_actor = DbActor::spawn(DbActor::new(db));
    db_actor
        .ask(DbExecute {
            sql: "INSERT INTO users (jid, account_id, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)"
                .to_owned(),
            params: waddle_server::db_params![
                "alice@waddle.social",
                alice_account.to_string(),
                "alice",
                "alice",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z"
            ],
        })
        .await
        .expect("seed alice user");
    let shutdown = CancellationToken::new();

    let handles = build_broker_runtime_from_config(
        db_actor.clone(),
        "waddle.social",
        &server_config.broker,
        Some(Arc::clone(&broker_perm_backend)),
        Arc::new(NoopRoomDisclosureEmitter),
        shutdown.clone(),
    )
    .await
    .expect("build broker runtime")
    .expect("broker enabled");

    // Verify all 3 persistent Turso broker databases exist under WADDLE_BROKER_DATA_DIR!
    assert!(broker_data_dir.join("admission.db").exists());
    assert!(broker_data_dir.join("custody.db").exists());
    assert!(broker_data_dir.join("destruction_ledger.db").exists());

    // 6. Enroll a tenant OpenRouter credential via BrokerCustodyService (backed by Scaleway KMS!)
    //    and verify ServerBrokerCredentialResolver resolves it by logical name AND by UUID!
    let tenant_cred = handles
        .custody
        .enroll(EnrollCredentialInput {
            tenant_id: default_tenant.clone(),
            account_id: None,
            provider: "openrouter".to_owned(),
            nonce: format!("tenant-or-{}", Uuid::new_v4()),
            payload_digest_sha256: [0x33; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::from_slice(b"sk-or-v1-live-tenant-key").expect("secret"),
        })
        .await
        .expect("enroll tenant openrouter credential via Scaleway KMS");

    let resolved_default = handles
        .resolver
        .resolve_tenant_credential(&default_tenant, "openrouter", "default")
        .await
        .expect("resolve tenant credential by logical name");
    assert_eq!(resolved_default, tenant_cred.credential_id);

    let resolved_uuid = handles
        .resolver
        .resolve_tenant_credential(
            &default_tenant,
            "openrouter",
            &tenant_cred.credential_id.to_string(),
        )
        .await
        .expect("resolve tenant credential by UUID");
    assert_eq!(resolved_uuid, tenant_cred.credential_id);

    // Unseal via Scaleway KMS decrypt and verify plaintext matches.
    let unsealed = handles
        .custody
        .unseal_for_provider_egress(&default_tenant, resolved_default, "openrouter")
        .await
        .expect("unseal via Scaleway KMS");
    assert_eq!(unsealed.as_bytes(), b"sk-or-v1-live-tenant-key");

    // Confirm Scaleway KMS mock actually served key creation, encrypt, and decrypt!
    {
        let st = scw_server.state.read().expect("lock");
        assert!(st.create_count >= 1, "Scaleway KMS key must be created");
        assert!(st.encrypt_count >= 1, "Scaleway KMS encrypt must be called");
        assert!(st.decrypt_count >= 1, "Scaleway KMS decrypt must be called");
    }

    // Revoking the tenant credential causes ServerBrokerCredentialResolver to deny.
    handles
        .custody
        .revoke_and_destroy(&default_tenant, tenant_cred.credential_id)
        .await
        .expect("revoke tenant credential");
    assert!(handles
        .resolver
        .resolve_tenant_credential(&default_tenant, "openrouter", "default")
        .await
        .is_err());

    // 7. Member credential enrollment + device-signed invocation resolution.
    let member_cred = handles
        .custody
        .enroll(EnrollCredentialInput {
            tenant_id: default_tenant.clone(),
            account_id: Some(alice_account),
            provider: "openrouter".to_owned(),
            nonce: format!("member-or-{}", Uuid::new_v4()),
            payload_digest_sha256: [0x44; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::from_slice(b"sk-or-v1-alice-member-key").expect("secret"),
        })
        .await
        .expect("enroll member credential");
    let inv_ctx = InvocationContext {
        tenant_id: Some(default_tenant.clone()),
        waddle_id: WaddleId::try_new("waddle.social").expect("waddle"),
        plugin_id: PluginId::try_new("ai-chatbot").expect("plugin"),
        requester: Some("alice@waddle.social".parse::<BareJid>().expect("jid")),
        source_room: Some(
            "general@muc.waddle.social"
                .parse::<BareJid>()
                .expect("room"),
        ),
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    };
    // Without a device-signed invocation, member resolution fails closed.
    let err_without_sig = handles
        .resolver
        .resolve_member_invocation(&default_tenant, &inv_ctx, "openrouter")
        .await
        .expect_err("must require device-signed invocation");
    assert!(err_without_sig.message.contains("device-signed"));

    // Register a signed invocation and verify resolution succeeds with the enrolled member credential!
    let signed = SignedInferenceInvocation {
        trust_domain: "waddle.social".to_owned(),
        tenant_id: default_tenant.clone(),
        account_id: alice_account,
        device_key_id: Uuid::new_v4(),
        extension_id: "ai-chatbot".to_owned(),
        extension_digest_sha256: [0x11; 32],
        room_scope: RoomScopeKey {
            scope_id: "general@muc.waddle.social".to_owned(),
        },
        provider: "openrouter".to_owned(),
        payer: PayerSelection::Member {
            account_id: alice_account,
            credential_id: member_cred.credential_id,
        },
        command_digest_sha256: [0x22; 32],
        nonce: [0x55; 32],
        expires_at_ms: Utc::now().timestamp_millis() + 60_000,
        budget: BudgetLimits {
            max_input_tokens: 4096,
            max_output_tokens: 1024,
            max_spend_microcents: 10_000,
            max_steps: 4,
        },
        signature: vec![0xAA; 64],
    };
    handles
        .resolver
        .register_member_invocation(default_tenant.clone(), alice_account, "openrouter", signed)
        .await;
    let (resolved_acct, resolved_cred_id, _signed) = handles
        .resolver
        .resolve_member_invocation(&default_tenant, &inv_ctx, "openrouter")
        .await
        .expect("resolve enrolled member credential + signed invocation");
    assert_eq!(resolved_acct, alice_account);
    assert_eq!(resolved_cred_id, member_cred.credential_id);

    // 8. Verify WADDLE_BROKER_ENABLED=false disables broker runtime cleanly.
    let disabled_cfg = BrokerRuntimeConfig::from_vars([("WADDLE_BROKER_ENABLED", "false")])
        .expect("disabled broker config");
    let disabled = build_broker_runtime_from_config(
        db_actor,
        "waddle.social",
        &disabled_cfg,
        Some(broker_perm_backend),
        Arc::new(NoopRoomDisclosureEmitter),
        shutdown.clone(),
    )
    .await
    .expect("disabled build");
    assert!(disabled.is_none());

    shutdown.cancel();
    // Yield briefly for background fence task shutdown.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = acme_tenant;
}

#[tokio::test]
async fn bootstrap_turso_tenant_registry_is_idempotent_across_restarts_and_preserves_data() {
    let fga_server = InProcessOpenFgaServer::start()
        .await
        .expect("start openfga");
    let tmp = TempDir::new().expect("tempdir");
    let turso_data_dir = tmp.path().join("turso-tenants");
    let tenants_dir = tmp.path().join("tenants");
    std::fs::create_dir_all(&tenants_dir).expect("create tenants dir");

    // Include a manifest for the default tenant itself (`waddle.social.json`)
    // alongside a second tenant to verify deduplication and restart idempotency.
    std::fs::write(
        tenants_dir.join("waddle.social.json"),
        json!({
            "tenantId": "waddle.social",
            "owners": ["rawkode", "icepuma", "randax"],
            "placementMode": "SharedPool"
        })
        .to_string(),
    )
    .expect("write default tenant manifest");
    std::fs::write(
        tenants_dir.join("acme.waddle.social.json"),
        json!({
            "tenantId": "acme.waddle.social",
            "owners": ["alice"],
            "placementMode": "SharedPool"
        })
        .to_string(),
    )
    .expect("write acme manifest");

    let manifests = load_tenant_manifests(&tenants_dir)
        .await
        .expect("load manifests");
    let extra_refs = extract_openfga_store_refs(&manifests).expect("extract refs");
    let default_tenant = TenantId::parse("waddle.social").expect("default tenant");
    let acme_tenant = TenantId::parse("acme.waddle.social").expect("acme tenant");
    let turso_cfg = TursoRuntimeConfig {
        data_dir: turso_data_dir.clone(),
        reserved_fds: 256,
        rlimit_nofile: Some(1024),
    };
    let server_config = ServerConfig {
        mode: ServerMode::HomeServer,
        base_url: "http://localhost:3000".to_owned(),
        session_key: "test-session-key-32-bytes-minimum".to_owned(),
        auth: AuthConfig::default(),
        extensions: waddle_extensions::ExtensionConfig::default(),
        link_preview: LinkPreviewConfig::default(),
        ws_keepalive: KeepaliveConfig::default(),
        max_websocket_connections: MaxWebSocketConnections::default(),
        spicedb: None,
        openfga: Some(OpenFgaConfig {
            endpoint: fga_server.endpoint().to_owned(),
            api_token: None,
            consistency: OpenFgaConsistencySetting::HigherConsistency,
            store_map: HashMap::new(),
        }),
        authz_backend: AuthzBackendSelection::Auto,
        broker: BrokerRuntimeConfig::default(),
        turso: Some(turso_cfg.clone()),
        tenancy: MultiTenantConfig {
            default_tenant_id: Some(default_tenant.clone()),
            tenants_dir: Some(tenants_dir.clone()),
        },
        occupant_id_secret: OccupantIdSecret::try_new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("occupant secret"),
        ingress: IngressConfig::default(),
        clustering: ClusteringConfig::default(),
        apns: None,
        fcm: None,
        trusted_web_origins: TrustedWebOrigins::try_new("http://localhost:3000", None)
            .expect("trusted origins"),
        host_grant_restore_epoch: RestoreEpoch::INITIAL,
    };

    let spiffe_id =
        SpiffeId::parse("spiffe://waddle.social/ns/waddle-next/sa/waddle").expect("spiffe");
    let keypair = HolderKeypair::generate().expect("keypair");
    let holder = keypair.sign_channel_binding(&spiffe_id, &[0x33; 32]);
    let scope = TenantQueryScope {
        tenant_id: default_tenant.clone(),
        holder,
        placement_generation: FenceGeneration::new(1),
    };
    let rawkode_account = AccountId::new_v4();

    // Boot #0: fresh PVC -> creates both DBs, registers OpenFGA stores, writes a tenant row.
    let boot0_authz_ref = {
        let (_actor, broker_perm_backend) =
            PermissionActor::from_server_config_with_broker_backend(
                &server_config,
                "waddle.social",
                &extra_refs,
            )
            .await
            .expect("openfga actor boot #0");
        let registry = bootstrap_turso_tenant_registry(
            &turso_cfg,
            &default_tenant,
            &manifests,
            broker_perm_backend,
        )
        .await
        .expect("bootstrap boot #0");
        assert!(turso_data_dir.join("waddle.social.db").exists());
        assert!(turso_data_dir.join("acme.waddle.social.db").exists());

        let guard = registry
            .acquire_tenant(&scope)
            .await
            .expect("acquire default tenant boot #0");
        let authz_ref = guard
            .authz_ref()
            .cloned()
            .expect("authz_ref populated on boot #0");
        registry
            .evict_tenant(&default_tenant)
            .await
            .expect("evict default tenant boot #0");
        authz_ref
    };

    // Persist a user row into waddle.social.db before restarting.
    {
        let store = TursoTenantStore::new(
            &turso_data_dir,
            TursoPoolConfig {
                rlimit_nofile_override: turso_cfg.rlimit_nofile,
                reserved_fds: turso_cfg.reserved_fds,
            },
            None,
        )
        .expect("direct turso store");
        assert!(matches!(
            store.provision_tenant(&default_tenant).await,
            Err(TenantStoreError::TenantAlreadyExists(_))
        ));
        store.open_tenant(&scope).await.expect("open direct store");
        store
            .upsert_user(
                &scope,
                &TenantUserRecord {
                    jid: "rawkode@waddle.social".to_owned(),
                    account_id: rawkode_account,
                    username: "rawkode".to_owned(),
                    xmpp_localpart: "rawkode".to_owned(),
                    display_name: Some("David".to_owned()),
                },
            )
            .await
            .expect("seed rawkode user");
        store
            .checkpoint_and_release(&default_tenant)
            .await
            .expect("checkpoint direct store");
    }

    // Boot #1 (pod restart): same PVC + same OpenFGA -> must succeed cleanly and preserve data!
    {
        let (_actor, broker_perm_backend) =
            PermissionActor::from_server_config_with_broker_backend(
                &server_config,
                "waddle.social",
                &extra_refs,
            )
            .await
            .expect("openfga actor boot #1");
        let registry = bootstrap_turso_tenant_registry(
            &turso_cfg,
            &default_tenant,
            &manifests,
            broker_perm_backend,
        )
        .await
        .expect("bootstrap boot #1 must succeed when tenant DBs already exist");

        let guard = registry
            .acquire_tenant(&scope)
            .await
            .expect("acquire default tenant boot #1");
        let boot1_authz_ref = guard
            .authz_ref()
            .cloned()
            .expect("authz_ref must remain populated after restart");
        assert_eq!(boot1_authz_ref.store_id, boot0_authz_ref.store_id);
        assert_eq!(boot1_authz_ref.model_version, boot0_authz_ref.model_version);

        // Verify the second manifest tenant also acquires cleanly after restart.
        let acme_scope = TenantQueryScope {
            tenant_id: acme_tenant.clone(),
            holder: scope.holder.clone(),
            placement_generation: FenceGeneration::new(1),
        };
        let acme_guard = registry
            .acquire_tenant(&acme_scope)
            .await
            .expect("acquire acme tenant boot #1");
        assert!(acme_guard.authz_ref().is_some());
        registry
            .evict_tenant(&default_tenant)
            .await
            .expect("evict default tenant boot #1");
    }

    // Confirm the user row written prior to restart is still intact.
    {
        let store = TursoTenantStore::new(
            &turso_data_dir,
            TursoPoolConfig {
                rlimit_nofile_override: turso_cfg.rlimit_nofile,
                reserved_fds: turso_cfg.reserved_fds,
            },
            None,
        )
        .expect("verify turso store");
        store.open_tenant(&scope).await.expect("open verify store");
        let user = store
            .get_user_by_localpart(&scope, "rawkode")
            .await
            .expect("query rawkode")
            .expect("rawkode user must survive restart");
        assert_eq!(user.account_id, rawkode_account);
        assert_eq!(user.display_name.as_deref(), Some("David"));
    }
}
