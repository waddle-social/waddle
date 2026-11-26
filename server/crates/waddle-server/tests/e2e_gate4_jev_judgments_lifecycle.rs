//! T4-S08: Release Evidence Gate 3/4 (M6) — Unattended tenant-funded
//! `jev-judgments` lifecycle: ZDR/no-training route qualification, opt-in
//! room observation via `InferenceCredentialRef::Tenant`, loopback TLS streaming
//! judgment batch, and host-verified `Completed` usage reconciliation.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use jid::BareJid;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
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
use waddle_broker::egress::{
    DataCollectionPolicy, ProviderOperation as ZdrProviderOp, QualifiedZdrRoute, RouteKeyOrigin,
    RouteQualificationError, ZdrRouteQuery, ZdrRouteRegistry,
};
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
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceError, HostInferenceErrorCode, HostInferenceMessage,
    HostInferenceRequest, InferenceCredentialRef, StreamItem,
};
use waddle_extensions::{PluginId, WaddleId};
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

struct JevTenantResolver {
    credential_id: Uuid,
}

#[async_trait]
impl BrokerCredentialResolver for JevTenantResolver {
    async fn resolve_requester_account(
        &self,
        _tenant_id: &TenantId,
        _requester: &BareJid,
    ) -> Result<(AccountId, AuthProvenance), HostInferenceError> {
        Err(HostInferenceError::new(
            HostInferenceErrorCode::Denied,
            "unattended jev observer has no interactive requester",
        ))
    }

    async fn resolve_tenant_credential(
        &self,
        _tenant_id: &TenantId,
        _provider: &str,
        credential_name: &str,
    ) -> Result<Uuid, HostInferenceError> {
        assert_eq!(credential_name, "default");
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
            "jev-judgments cannot invoke member credentials",
        ))
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
            let headers = String::from_utf8_lossy(&buf[..headers_end]);
            let content_len = headers
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    if k.eq_ignore_ascii_case("content-length") {
                        v.trim().parse::<usize>().ok()
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

#[tokio::test]
async fn gate4_unattended_jev_judgments_zdr_and_tenant_lifecycle() {
    // 1. ZDR/no-training route qualification succeeds for ManagedTenant and rejects non-ZDR.
    let mut zdr = ZdrRouteRegistry::new();
    zdr.register_qualified(QualifiedZdrRoute {
        provider: "openrouter".to_owned(),
        tier: "enterprise-zdr".to_owned(),
        deployment: "default".to_owned(),
        model: "typesafe/jev-1.13".to_owned(),
        operation: ZdrProviderOp::ChatCompletions,
        downstream_routing: "openrouter-zdr-strict".to_owned(),
        data_collection: DataCollectionPolicy::Deny,
        zdr: true,
        allow_fallback: false,
        qualified_until_ms: 4_000_000_000_000,
    })
    .expect("qualified route");
    zdr.select_route(&ZdrRouteQuery {
        provider: "openrouter",
        tier: "enterprise-zdr",
        deployment: "default",
        model: "typesafe/jev-1.13",
        operation: ZdrProviderOp::ChatCompletions,
        downstream_routing: "openrouter-zdr-strict",
        key_origin: RouteKeyOrigin::ManagedTenant,
        now_ms: 1_750_000_000_000,
    })
    .expect("ZDR route selects");
    assert_eq!(
        zdr.select_route(&ZdrRouteQuery {
            provider: "openrouter",
            tier: "standard",
            deployment: "default",
            model: "typesafe/jev-1.13",
            operation: ZdrProviderOp::ChatCompletions,
            downstream_routing: "openrouter-default",
            key_origin: RouteKeyOrigin::ManagedTenant,
            now_ms: 1_750_000_000_000,
        }),
        Err(RouteQualificationError::UnknownRoute)
    );

    // 2. Unattended RoomMessageObserve streams via tenant credential through loopback TLS.
    let tls = generate_test_tls_material("openrouter.acme.test");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);

    let srv = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut tls_stream = acceptor.accept(tcp).await.expect("tls accept");
        let req = read_http_request(&mut tls_stream).await;
        assert!(req.contains("sk-tenant-jev-zdr-key"));
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"scores\\\":[0,0,0,0,0,0,0,0]}\"}}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":33,\"completion_tokens\":14}}\n\n",
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

    let dir = TempDir::new().expect("tempdir");
    let tenant_id = TenantId::parse("acme.example").expect("tenant");
    let spiffe_id =
        SpiffeId::parse("spiffe://acme.example/ns/cells/sa/shared-pool").expect("spiffe");
    let keypair = HolderKeypair::generate().expect("keypair");
    let channel_binding = [0x7Au8; 32];
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
    replica_fence.tick_once().await.expect("tick");

    let custody_ledger = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("custody"))
            .await
            .expect("custody"),
    );
    let kms: Arc<dyn KmsProvider> = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("acme.example", kms));
    let custody = Arc::new(
        BrokerCustodyService::new(hierarchy, custody_ledger)
            .await
            .expect("custody"),
    );
    let cred = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant_id.clone(),
            account_id: None,
            provider: "openrouter".to_owned(),
            nonce: format!("nonce-{}", Uuid::new_v4()),
            payload_digest_sha256: [4u8; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(b"sk-tenant-jev-zdr-key".to_vec()).expect("secret"),
        })
        .await
        .expect("enroll");

    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    perms
        .provision_tenant_store(&tenant_id)
        .await
        .expect("provision");
    perms
        .write_tuples(
            &tenant_id,
            AuthzModule::Inference,
            &[AuthzTuple {
                object: AuthzObject {
                    object_type: AuthzObjectType::TenantCredential,
                    id: cred.credential_id.to_string(),
                },
                relation: AuthzRelation::new("use"),
                subject: AuthzSubject {
                    subject_type: AuthzSubjectType::Extension,
                    id: "observation".to_owned(),
                    relation: None,
                },
            }],
        )
        .await
        .expect("tuple");

    let plugin_id = PluginId::try_new("jev-judgments").expect("plugin");
    let mut ext_digest = [0u8; 32];
    ext_digest.copy_from_slice(&Sha256::digest(plugin_id.as_str().as_bytes()));
    let mut profile_registry = EgressProfileRegistry::new();
    profile_registry
        .register_with_policy(
            ProviderProfile {
                provider: "openrouter".to_owned(),
                deployment: "default".to_owned(),
                model: "typesafe/jev-1.13".to_owned(),
                operation: ProviderOperation::ChatCompletions,
                base_origin: "https://openrouter.acme.test".to_owned(),
                operation_path: "/api/v1/chat/completions".to_owned(),
                caps: EgressCaps {
                    max_request_bytes: 64 * 1024,
                    max_response_bytes: 1024 * 1024,
                    max_chunk_bytes: 64 * 1024,
                    request_timeout_ms: 5_000,
                    max_concurrent_requests: 4,
                    max_spend_microcents: 50_000,
                },
                pinned_ca_pem: Some(tls.ca_pem),
            },
            EgressBindingPolicy {
                expected_trust_domain: Some("acme.example".to_owned()),
                allowed_extension_ids: vec!["jev-judgments".to_owned()],
                allowed_extension_digests: vec![ext_digest],
            },
        )
        .expect("profile");

    let egress = Arc::new(BrokerEgressClient::with_loopback_tls_test_override(
        profile_registry,
        custody,
        Arc::new(MockDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
                443,
            )],
        }),
        addr,
    ));
    let admission = Arc::new(BrokerAdmissionService::new(
        "broker-gate4-jev",
        Arc::clone(&ledger),
        perms,
        Arc::new(DeviceKeyRegistry::new()),
        replica_fence,
        TraceRecorder::new(),
    ));
    let host = BrokerExtensionInferenceHost::new(
        BrokerExtensionInferenceHostConfig {
            trust_domain: "acme.example".to_owned(),
            holder,
            peer_spiffe_id: spiffe_id,
            channel_binding,
            default_budget: BudgetLimits {
                max_input_tokens: 4_096,
                max_output_tokens: 1_024,
                max_spend_microcents: 50_000,
                max_steps: 4,
            },
            default_deployment: "default".to_owned(),
            default_reserved_spend_microcents: 180,
        },
        admission,
        egress,
        Arc::new(JevTenantResolver {
            credential_id: cred.credential_id,
        }),
    );

    let mut opened = host
        .start_stream(
            &InvocationContext {
                tenant_id: Some(tenant_id),
                waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
                plugin_id,
                requester: None,
                source_room: Some("optin-room@muc.acme.example".parse().expect("room")),
                kind: InvocationKind::RoomMessageObserve,
                provider_room_grants: Vec::new(),
            },
            HostInferenceRequest {
                credential: InferenceCredentialRef::Tenant("default".to_owned()),
                provider: "openrouter".to_owned(),
                deployment: "default".to_owned(),
                model: "typesafe/jev-1.13".to_owned(),
                messages: vec![HostInferenceMessage {
                    role: "user".to_owned(),
                    content: "evaluate message safety".to_owned(),
                }],
                max_output_tokens: Some(128),
            },
        )
        .await
        .expect("jev tenant inference must admit and stream");

    let mut usage = None;
    let mut raw = String::new();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(5), opened.rx.recv())
        .await
        .expect("stream timeout")
    {
        match item {
            StreamItem::Chunk(c) => raw.push_str(&String::from_utf8_lossy(&c.delta)),
            StreamItem::EndOfStream(u) => {
                usage = Some(u);
                break;
            }
            StreamItem::Error(err) => panic!("unexpected stream error: {err:?}"),
        }
    }
    srv.await.expect("server");
    assert!(raw.contains("scores"));
    let u = usage.expect("usage");
    assert_eq!(u.input_tokens, 33);
    assert_eq!(u.output_tokens, 14);

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite:{}",
            dir.path().join("admission.db").display()
        ))
        .await
        .expect("connect db");
    let row = sqlx::query("SELECT attempt_id FROM admission_attempts LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("fetch attempt");
    let attempt_id: String = sqlx::Row::get(&row, 0);
    let rec = ledger
        .get_attempt(AttemptId::from_uuid(
            Uuid::parse_str(&attempt_id).expect("uuid"),
        ))
        .await
        .expect("get attempt")
        .expect("attempt row");
    assert_eq!(rec.status, AttemptStatus::Completed);
}
