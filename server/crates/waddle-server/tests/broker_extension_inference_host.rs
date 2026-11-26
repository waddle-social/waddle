//! Integration tests exercising `BrokerExtensionInferenceHost` directly against
//! `BrokerAdmissionService`, `BrokerEgressClient`, `TursoAdmissionLedgerStore`,
//! holder SPIFFE/tls-exporter verification, canonical account resolution, and
//! `ExtensionManager::from_config_for_tenant_with_hosts`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use jid::BareJid;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use sqlx::Row;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;
use waddle_broker::admission::replica_fence::BrokerReplicaFence;
use waddle_broker::admission::service::{BrokerAdmissionService, TraceRecorder};
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::{AdmissionQueryStore, AttemptStatus, ReplicaFenceStore};
use waddle_broker::authz::local::LocalPermissionBackend;
use waddle_broker::authz::AuthzCallerRole;
use waddle_broker::custody::{BrokerCustodyService, EnrollCredentialInput};
use waddle_broker::egress::client::BrokerEgressClient;
use waddle_broker::egress::profile::{
    EgressBindingPolicy, EgressCaps, EgressProfileRegistry, ProviderOperation, ProviderProfile,
};
use waddle_broker::egress::ssrf::{DnsResolver, SsrfError};
use waddle_broker::idp_proof::{AuthProvenance, DeviceKeyRegistry};
use waddle_broker::key_hierarchy::KeyHierarchyService;
use waddle_broker::kms::local::LocalKmsProvider;
use waddle_broker::kms::KmsProvider;
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::secret::SecretBytes;
use waddle_broker_core::admission::{AttemptId, BudgetLimits};
use waddle_broker_core::boot_clock::SystemBootClock;
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{AccountId, SpiffeId, TenantId};
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    AuthzTuple, PermissionBackend,
};
use waddle_broker_core::quotas::{TenantResourceController, TenantResourceQuotas};
use waddle_extensions::config::{ExtensionConfig, ExtensionModuleConfig, RuntimeLimits};
use waddle_extensions::host_tools::{DenyingExtensionHostTools, InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceError, HostInferenceErrorCode, HostInferenceMessage,
    HostInferenceRequest, InferenceCredentialRef, StreamItem,
};
use waddle_extensions::{ExtensionCapability, ExtensionManager, PluginId, WaddleId};
use waddle_server::server::extension_inference_host::{
    BrokerCredentialResolver, BrokerExtensionInferenceHost, BrokerExtensionInferenceHostConfig,
};

struct MockDnsResolver {
    addrs: Vec<SocketAddr>,
}

#[async_trait]
impl DnsResolver for MockDnsResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, SsrfError> {
        Ok(self.addrs.clone())
    }
}

struct TestTlsCertMaterial {
    ca_pem: Vec<u8>,
    server_config: Arc<rustls::ServerConfig>,
}

fn generate_test_tls_material(domain: &str) -> TestTlsCertMaterial {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().expect("ca key");
    let ca_cert = ca_params.self_signed(&ca_key).expect("ca self signed");
    let ca_pem = ca_cert.pem().into_bytes();

    let leaf_params = CertificateParams::new(vec![domain.to_owned()]).expect("leaf params");
    let leaf_key = KeyPair::generate().expect("leaf key");
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .expect("leaf signed");

    let cert_der: CertificateDer<'static> = leaf_cert.der().clone();
    let ca_der: CertificateDer<'static> = ca_cert.der().clone();
    let key_der = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("tls versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der, ca_der], key_der)
        .expect("server config");

    TestTlsCertMaterial {
        ca_pem,
        server_config: Arc::new(server_config),
    }
}

async fn read_http_request(
    stream: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let n = stream.read(&mut tmp).await.expect("read tls request");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(headers_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let header_text = String::from_utf8_lossy(&buf[..headers_end]);
            let content_len = header_text
                .lines()
                .find_map(|line| {
                    let (name, val) = line.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        val.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            if buf.len() >= headers_end + 4 + content_len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

struct StubCredentialResolver {
    expected_requester: BareJid,
    account_id: AccountId,
    credential_id: Uuid,
    resolved_requesters: Mutex<Vec<BareJid>>,
}

#[async_trait]
impl BrokerCredentialResolver for StubCredentialResolver {
    async fn resolve_requester_account(
        &self,
        _tenant_id: &TenantId,
        requester: &BareJid,
    ) -> Result<(AccountId, AuthProvenance), HostInferenceError> {
        self.resolved_requesters
            .lock()
            .await
            .push(requester.clone());
        if *requester == self.expected_requester {
            Ok((self.account_id, AuthProvenance::ExternalIdp))
        } else {
            Err(HostInferenceError::new(
                HostInferenceErrorCode::Denied,
                "unknown requester",
            ))
        }
    }

    async fn resolve_tenant_credential(
        &self,
        _tenant_id: &TenantId,
        _provider: &str,
        _credential_name: &str,
    ) -> Result<Uuid, HostInferenceError> {
        Ok(self.credential_id)
    }

    async fn resolve_member_invocation(
        &self,
        _tenant_id: &TenantId,
        _context: &InvocationContext,
        _provider: &str,
    ) -> Result<
        (
            AccountId,
            Uuid,
            waddle_broker::admission::invocation::SignedInferenceInvocation,
        ),
        HostInferenceError,
    > {
        Err(HostInferenceError::new(
            HostInferenceErrorCode::Denied,
            "member invocation not configured in test",
        ))
    }
}

struct TestHostEnv {
    _dir: TempDir,
    tenant_id: TenantId,
    account_id: AccountId,
    requester: BareJid,
    spiffe_id: SpiffeId,
    channel_binding: [u8; 32],
    ledger: Arc<TursoAdmissionLedgerStore>,
    resolver: Arc<StubCredentialResolver>,
    host: BrokerExtensionInferenceHost<TursoAdmissionLedgerStore>,
    plugin_id: PluginId,
}

async fn setup_test_host(
    server_addr: SocketAddr,
    ca_pem: Vec<u8>,
    binding_override: Option<[u8; 32]>,
    peer_spiffe_override: Option<SpiffeId>,
) -> TestHostEnv {
    let dir = TempDir::new().expect("tempdir");
    let tenant_id = TenantId::parse("acme.example").expect("tenant");
    let account_id = AccountId::new_v4();
    let requester: BareJid = "alice@acme.example".parse().expect("requester");
    let spiffe_id =
        SpiffeId::parse("spiffe://acme.example/ns/cells/sa/shared-pool").expect("spiffe");
    let keypair = HolderKeypair::generate().expect("keypair");
    let channel_binding = [0x42u8; 32];
    let holder = keypair.sign_channel_binding(&spiffe_id, &channel_binding);

    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("admission.db"))
            .await
            .expect("admission db"),
    );
    let replica_fence = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn ReplicaFenceStore>,
        Arc::new(SystemBootClock),
    ));
    replica_fence.tick_once().await.expect("tick_once");

    let custody_ledger = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("custody"))
            .await
            .expect("custody db"),
    );
    let kms: Arc<dyn KmsProvider> = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("acme.example", kms));
    let custody = Arc::new(
        BrokerCustodyService::new(hierarchy, custody_ledger)
            .await
            .expect("custody service"),
    );
    let cred_receipt = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant_id.clone(),
            account_id: None,
            provider: "openai".to_owned(),
            nonce: format!("nonce-{}", Uuid::new_v4()),
            payload_digest_sha256: [7u8; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(b"sk-test-broker-secret".to_vec()).expect("secret"),
        })
        .await
        .expect("enroll cred");

    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    perms
        .provision_tenant_store(&tenant_id)
        .await
        .expect("provision store");
    perms
        .write_tuples(
            &tenant_id,
            AuthzModule::Inference,
            &[AuthzTuple {
                object: AuthzObject {
                    object_type: AuthzObjectType::TenantCredential,
                    id: cred_receipt.credential_id.to_string(),
                },
                relation: AuthzRelation::new("use"),
                subject: AuthzSubject {
                    subject_type: AuthzSubjectType::Account,
                    id: account_id.to_string(),
                    relation: None,
                },
            }],
        )
        .await
        .expect("write inference tuple");

    let plugin_id = PluginId::try_new("ai-chatbot").expect("plugin id");
    let mut profile_registry = EgressProfileRegistry::new();
    let mut ext_digest = [0u8; 32];
    ext_digest.copy_from_slice(&Sha256::digest(plugin_id.as_str().as_bytes()));
    profile_registry
        .register_with_policy(
            ProviderProfile {
                provider: "openai".to_owned(),
                deployment: "prod".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperation::ChatCompletions,
                base_origin: "https://provider.acme.test".to_owned(),
                operation_path: "/v1/chat/completions".to_owned(),
                caps: EgressCaps {
                    max_request_bytes: 64 * 1024,
                    max_response_bytes: 1024 * 1024,
                    max_chunk_bytes: 64 * 1024,
                    request_timeout_ms: 5_000,
                    max_concurrent_requests: 4,
                    max_spend_microcents: 50_000,
                },
                pinned_ca_pem: Some(ca_pem),
            },
            EgressBindingPolicy {
                expected_trust_domain: Some("acme.example".to_owned()),
                allowed_extension_ids: vec!["ai-chatbot".to_owned()],
                allowed_extension_digests: vec![ext_digest],
            },
        )
        .expect("register profile");

    let egress = Arc::new(BrokerEgressClient::with_loopback_tls_test_override(
        profile_registry,
        custody,
        Arc::new(MockDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
                443,
            )],
        }),
        server_addr,
    ));

    let admission = Arc::new(BrokerAdmissionService::new(
        "broker-test",
        Arc::clone(&ledger),
        perms,
        Arc::new(DeviceKeyRegistry::new()),
        replica_fence,
        TraceRecorder::new(),
    ));

    let resolver = Arc::new(StubCredentialResolver {
        expected_requester: requester.clone(),
        account_id,
        credential_id: cred_receipt.credential_id,
        resolved_requesters: Mutex::new(Vec::new()),
    });

    let host = BrokerExtensionInferenceHost::new(
        BrokerExtensionInferenceHostConfig {
            trust_domain: "acme.example".to_owned(),
            holder,
            peer_spiffe_id: peer_spiffe_override.unwrap_or_else(|| spiffe_id.clone()),
            channel_binding: binding_override.unwrap_or(channel_binding),
            default_budget: BudgetLimits {
                max_input_tokens: 4_096,
                max_output_tokens: 1_024,
                max_spend_microcents: 50_000,
                max_steps: 4,
            },
            default_deployment: "prod".to_owned(),
            default_reserved_spend_microcents: 250,
        },
        admission,
        egress,
        Arc::clone(&resolver) as Arc<dyn BrokerCredentialResolver>,
    );

    TestHostEnv {
        _dir: dir,
        tenant_id,
        account_id,
        requester,
        spiffe_id,
        channel_binding,
        ledger,
        resolver,
        host,
        plugin_id,
    }
}

#[tokio::test]
async fn broker_extension_inference_host_streams_resolves_account_and_reconciles_usage() {
    let tls = generate_test_tls_material("provider.acme.test");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);

    let srv = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut tls_stream = acceptor.accept(tcp).await.expect("tls accept");
        let req = read_http_request(&mut tls_stream).await;
        assert!(req.contains("sk-test-broker-secret"));
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":17}}\n\n",
            "data: [DONE]\n\n"
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        tls_stream.write_all(resp.as_bytes()).await.expect("write");
        tls_stream.shutdown().await.expect("shutdown");
    });

    let env = setup_test_host(addr, tls.ca_pem, None, None).await;
    let context = InvocationContext {
        tenant_id: Some(env.tenant_id.clone()),
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        plugin_id: env.plugin_id.clone(),
        requester: Some(env.requester.clone()),
        source_room: Some("room-1@muc.acme.example".parse().expect("room")),
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    };
    let req = HostInferenceRequest {
        credential: InferenceCredentialRef::Tenant("default".to_owned()),
        provider: "openai".to_owned(),
        deployment: "prod".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        messages: vec![HostInferenceMessage {
            role: "user".to_owned(),
            content: "hello broker".to_owned(),
        }],
        max_output_tokens: Some(128),
    };

    let mut opened = env
        .host
        .start_stream(&context, req)
        .await
        .expect("start_stream");
    let mut terminal_usage = None;
    let mut chunk_count = 0usize;
    while let Some(item) = opened.rx.recv().await {
        match item {
            StreamItem::Chunk(_) => chunk_count += 1,
            StreamItem::EndOfStream(usage) => {
                terminal_usage = Some(usage);
                break;
            }
            StreamItem::Error(err) => panic!("unexpected stream error: {err:?}"),
        }
    }
    assert!(chunk_count >= 1);
    let usage = terminal_usage.expect("terminal usage emitted");
    assert_eq!(usage.input_tokens, 42);
    assert_eq!(usage.output_tokens, 17);
    assert_eq!(usage.cost_microcents, 250);

    let resolved = env.resolver.resolved_requesters.lock().await;
    assert_eq!(resolved.as_slice(), &[env.requester]);

    // Verify the admission ledger reconciled the attempt with real usage (Completed).
    let attempt_id_str: String = {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!(
                "sqlite://{}",
                env._dir.path().join("admission.db").display()
            ))
            .await
            .expect("open admission sqlite");
        let row = sqlx::query("SELECT attempt_id FROM admission_attempts LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("query attempt_id");
        row.get(0)
    };
    let attempt_id = AttemptId::from_uuid(Uuid::parse_str(&attempt_id_str).expect("uuid"));
    let record = env
        .ledger
        .get_attempt(attempt_id)
        .await
        .expect("get_attempt")
        .expect("attempt record");
    assert_eq!(record.status, AttemptStatus::Completed);
    assert_eq!(record.input_tokens, 42);
    assert_eq!(record.output_tokens, 17);
    assert_eq!(record.actual_spend_microcents, 250);
    assert_ne!(env.account_id.as_uuid(), Uuid::nil());

    srv.await.expect("mock server");
}

#[tokio::test]
async fn broker_extension_inference_host_rejects_unauthenticated_holder_and_missing_requester() {
    let tls = generate_test_tls_material("provider.acme.test");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    // 1. Channel binding mismatch -> HolderInstance verification fails closed.
    let bad_binding_env = setup_test_host(addr, tls.ca_pem.clone(), Some([0x99u8; 32]), None).await;
    let context = InvocationContext {
        tenant_id: Some(bad_binding_env.tenant_id.clone()),
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        plugin_id: bad_binding_env.plugin_id.clone(),
        requester: Some(bad_binding_env.requester.clone()),
        source_room: Some("room-1@muc.acme.example".parse().expect("room")),
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    };
    let req = HostInferenceRequest {
        credential: InferenceCredentialRef::Tenant("default".to_owned()),
        provider: "openai".to_owned(),
        deployment: "prod".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        messages: vec![HostInferenceMessage {
            role: "user".to_owned(),
            content: "hello".to_owned(),
        }],
        max_output_tokens: None,
    };
    let err = match bad_binding_env
        .host
        .start_stream(&context, req.clone())
        .await
    {
        Ok(_) => panic!("mismatched channel binding must fail"),
        Err(err) => err,
    };
    assert_eq!(err.code, HostInferenceErrorCode::Denied);
    assert!(bad_binding_env
        .resolver
        .resolved_requesters
        .lock()
        .await
        .is_empty());

    // 2. Peer SPIFFE ID mismatch -> HolderInstance verification fails closed.
    let wrong_spiffe =
        SpiffeId::parse("spiffe://acme.example/ns/cells/sa/dedicated-other").expect("spiffe");
    let bad_spiffe_env = setup_test_host(addr, tls.ca_pem.clone(), None, Some(wrong_spiffe)).await;
    assert_ne!(bad_spiffe_env.spiffe_id.as_str(), "");
    assert_eq!(bad_spiffe_env.channel_binding, [0x42u8; 32]);
    let err2 = match bad_spiffe_env
        .host
        .start_stream(&context, req.clone())
        .await
    {
        Ok(_) => panic!("mismatched SPIFFE ID must fail"),
        Err(err) => err,
    };
    assert_eq!(err2.code, HostInferenceErrorCode::Denied);

    // 3. Missing requester on interactive command -> denied without fabricating AccountId.
    let valid_env = setup_test_host(addr, tls.ca_pem, None, None).await;
    let missing_req_ctx = InvocationContext {
        requester: None,
        ..context
    };
    let err3 = match valid_env.host.start_stream(&missing_req_ctx, req).await {
        Ok(_) => panic!("missing requester on Command must fail"),
        Err(err) => err,
    };
    assert_eq!(err3.code, HostInferenceErrorCode::Denied);
}

#[tokio::test]
async fn broker_extension_inference_host_reconciles_canceled_on_stream_cancel() {
    let tls = generate_test_tls_material("provider.acme.test");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);
    let disconnected = Arc::new(Notify::new());
    let disconnected_srv = Arc::clone(&disconnected);
    let requests_served = Arc::new(AtomicUsize::new(0));
    let requests_srv = Arc::clone(&requests_served);

    let srv = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        requests_srv.fetch_add(1, Ordering::SeqCst);
        let mut tls_stream = acceptor.accept(tcp).await.expect("tls accept");
        let _req = read_http_request(&mut tls_stream).await;
        let first_chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"part\"}}]}\n\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n",
            first_chunk.len(),
            first_chunk
        );
        tls_stream.write_all(resp.as_bytes()).await.expect("write");
        let mut buf = [0u8; 64];
        let _ = tls_stream.read(&mut buf).await;
        disconnected_srv.notify_one();
    });

    let env = setup_test_host(addr, tls.ca_pem, None, None).await;
    let context = InvocationContext {
        tenant_id: Some(env.tenant_id.clone()),
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        plugin_id: env.plugin_id.clone(),
        requester: Some(env.requester.clone()),
        source_room: Some("room-1@muc.acme.example".parse().expect("room")),
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    };
    let req = HostInferenceRequest {
        credential: InferenceCredentialRef::Tenant("default".to_owned()),
        provider: "openai".to_owned(),
        deployment: "prod".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        messages: vec![HostInferenceMessage {
            role: "user".to_owned(),
            content: "cancel me".to_owned(),
        }],
        max_output_tokens: Some(64),
    };

    let mut opened = env
        .host
        .start_stream(&context, req)
        .await
        .expect("start_stream");
    let first = opened.rx.recv().await.expect("first chunk");
    assert!(matches!(first, StreamItem::Chunk(_)));

    opened.cancel.cancel_sync();
    tokio::time::timeout(Duration::from_secs(3), disconnected.notified())
        .await
        .expect("upstream socket closed on cancel");
    srv.await.expect("srv");
    assert_eq!(requests_served.load(Ordering::SeqCst), 1);

    // Pump task reconciles the attempt as Canceled in TursoAdmissionLedgerStore.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let attempt_id_str: String = {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!(
                "sqlite://{}",
                env._dir.path().join("admission.db").display()
            ))
            .await
            .expect("open admission sqlite");
        let row = sqlx::query("SELECT attempt_id FROM admission_attempts LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("query attempt_id");
        row.get(0)
    };
    let attempt_id = AttemptId::from_uuid(Uuid::parse_str(&attempt_id_str).expect("uuid"));
    let record = env
        .ledger
        .get_attempt(attempt_id)
        .await
        .expect("get_attempt")
        .expect("record");
    assert_eq!(record.status, AttemptStatus::Canceled);
}

#[tokio::test]
async fn extension_manager_for_tenant_binds_canonical_tenant_id_on_actors() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../waddle-extensions/tests/fixtures/inference_stream.wat");
    let cache_dir = TempDir::new().expect("tempdir");
    let tenant_id = TenantId::parse("acme.example").expect("tenant");
    let controller =
        Arc::new(TenantResourceController::new(TenantResourceQuotas::default()).expect("quotas"));

    struct RecordingHost {
        seen_tenants: Mutex<Vec<Option<TenantId>>>,
    }

    #[async_trait]
    impl ExtensionInferenceHost for RecordingHost {
        async fn start_stream(
            &self,
            context: &InvocationContext,
            _request: HostInferenceRequest,
        ) -> Result<waddle_extensions::runtime::OpenedInferenceStream, HostInferenceError> {
            self.seen_tenants
                .lock()
                .await
                .push(context.tenant_id.clone());
            Err(HostInferenceError::new(
                HostInferenceErrorCode::Denied,
                "recorded tenant_id",
            ))
        }
    }

    let host = Arc::new(RecordingHost {
        seen_tenants: Mutex::new(Vec::new()),
    });
    let mgr = ExtensionManager::from_config_for_tenant_with_hosts(
        tenant_id.clone(),
        ExtensionConfig {
            enabled: true,
            cache_dir: cache_dir.path().display().to_string(),
            modules: vec![ExtensionModuleConfig {
                name: "ai-chatbot".to_owned(),
                room_observation: None,
                runtime_limits: RuntimeLimits::default(),
                registry: String::new(),
                digest: None,
                tag: None,
                namespace: "urn:waddle:ai-chatbot:1".to_owned(),
                config: serde_json::json!({}),
                capability_grants: vec![ExtensionCapability::Commands],
                allowed_http_origins: Vec::new(),
                provider_room_grants: Vec::new(),
                config_secret_files: Default::default(),
                local_path: Some(fixture_path.display().to_string()),
            }],
        },
        Arc::new(DenyingExtensionHostTools),
        Arc::clone(&host) as Arc<dyn ExtensionInferenceHost>,
        controller,
    )
    .await
    .expect("extension manager");

    assert_eq!(mgr.tenant_id(), Some(&tenant_id));
    let _effects = mgr
        .invoke_command(waddle_extensions::manager::CommandInvocationRequest {
            node: "urn:waddle:extension:1:ai-chatbot",
            waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
            room: None,
            requester: waddle_extensions::FullJidValue::try_new(
                "alice@acme.example/web".to_owned(),
            )
            .expect("requester"),
            session_id: None,
            action: Some(waddle_extensions::CommandAction::Execute),
            fields: Vec::new(),
            form: None,
        })
        .await;
    let seen = host.seen_tenants.lock().await;
    assert_eq!(seen.as_slice(), &[Some(tenant_id)]);
}
