//! T3-09: Pairwise (F17 + F18 + F20) — Hostile upstream provider responses
//! (302 SSRF redirect + 502 error body containing provider secret sentinel)
//! fail closed with zero secret or metadata leak across error values and traces.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

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
use waddle_broker::admission::ReplicaFenceStore;
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
use waddle_broker_core::admission::BudgetLimits;
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
    HostInferenceRequest, InferenceCredentialRef,
};
use waddle_extensions::{PluginId, WaddleId};
use waddle_server::server::extension_inference_host::{
    BrokerCredentialResolver, BrokerExtensionInferenceHost, BrokerExtensionInferenceHostConfig,
};

const CRED_SENTINEL: &str = "sk-waddle-hostile-cred-sentinel-8812";
const BODY_SENTINEL: &str = "LEAK_UPSTREAM_BODY_SENTINEL_7741";
const REDIRECT_SENTINEL: &str = "http://169.254.169.254/latest/meta-data/";

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

struct StubResolver {
    account_id: AccountId,
    credential_id: Uuid,
}

#[async_trait]
impl BrokerCredentialResolver for StubResolver {
    async fn resolve_requester_account(
        &self,
        _tenant_id: &TenantId,
        _requester: &BareJid,
    ) -> Result<(AccountId, AuthProvenance), HostInferenceError> {
        Ok((self.account_id, AuthProvenance::ExternalIdp))
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
            "unused",
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
async fn hostile_provider_302_metadata_and_502_sentinel_never_leak_secrets() {
    let tls = generate_test_tls_material("provider.acme.test");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);

    let srv = tokio::spawn(async move {
        // Request 1: hostile 302 redirect to link-local metadata endpoint.
        let (tcp1, _) = listener.accept().await.expect("accept 1");
        let mut s1 = acceptor.accept(tcp1).await.expect("tls 1");
        let _ = read_http_request(&mut s1).await;
        let r1 = format!(
            "HTTP/1.1 302 Found\r\nLocation: {REDIRECT_SENTINEL}?token={CRED_SENTINEL}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        s1.write_all(r1.as_bytes()).await.expect("write 1");
        s1.shutdown().await.expect("shutdown 1");

        // Request 2: hostile 502 echoing upstream secret sentinel in body.
        let (tcp2, _) = listener.accept().await.expect("accept 2");
        let mut s2 = acceptor.accept(tcp2).await.expect("tls 2");
        let _ = read_http_request(&mut s2).await;
        let body2 = format!("{{\"error\":\"{CRED_SENTINEL} {BODY_SENTINEL}\"}}");
        let r2 = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body2.len(),
            body2
        );
        s2.write_all(r2.as_bytes()).await.expect("write 2");
        s2.shutdown().await.expect("shutdown 2");
    });

    let dir = TempDir::new().expect("tempdir");
    let tenant_id = TenantId::parse("acme.example").expect("tenant");
    let account_id = AccountId::new_v4();
    let spiffe_id =
        SpiffeId::parse("spiffe://acme.example/ns/cells/sa/shared-pool").expect("spiffe");
    let keypair = HolderKeypair::generate().expect("keypair");
    let channel_binding = [0x33u8; 32];
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
            .expect("custody db"),
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
            provider: "openai".to_owned(),
            nonce: format!("nonce-{}", Uuid::new_v4()),
            payload_digest_sha256: [5u8; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(CRED_SENTINEL.as_bytes().to_vec()).expect("secret"),
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
                    subject_type: AuthzSubjectType::Account,
                    id: account_id.to_string(),
                    relation: None,
                },
            }],
        )
        .await
        .expect("tuple");

    let plugin_id = PluginId::try_new("ai-chatbot").expect("plugin");
    let mut ext_digest = [0u8; 32];
    ext_digest.copy_from_slice(&Sha256::digest(plugin_id.as_str().as_bytes()));
    let mut profile_registry = EgressProfileRegistry::new();
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
                pinned_ca_pem: Some(tls.ca_pem),
            },
            EgressBindingPolicy {
                expected_trust_domain: Some("acme.example".to_owned()),
                allowed_extension_ids: vec!["ai-chatbot".to_owned()],
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
    let trace = TraceRecorder::new();
    let admission = Arc::new(BrokerAdmissionService::new(
        "broker-hostile-test",
        ledger,
        perms,
        Arc::new(DeviceKeyRegistry::new()),
        replica_fence,
        trace.clone(),
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
            default_deployment: "prod".to_owned(),
            default_reserved_spend_microcents: 250,
        },
        admission,
        egress,
        Arc::new(StubResolver {
            account_id,
            credential_id: cred.credential_id,
        }),
    );

    let ctx = InvocationContext {
        tenant_id: Some(tenant_id),
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        plugin_id,
        requester: Some("alice@acme.example".parse().expect("alice")),
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
            content: "hi".to_owned(),
        }],
        max_output_tokens: Some(64),
    };

    let err1 = host
        .start_stream(&ctx, req.clone())
        .await
        .err()
        .expect("302 redirect must fail closed");
    let err2 = host
        .start_stream(&ctx, req)
        .await
        .err()
        .expect("502 response must fail closed");
    srv.await.expect("server");

    let combined = format!("{}\n{}\n{}", err1.message, err2.message, trace.to_jsonl());
    for sentinel in [CRED_SENTINEL, BODY_SENTINEL, REDIRECT_SENTINEL] {
        assert!(
            !combined.contains(sentinel),
            "hostile provider leak detected for {sentinel}: {combined}"
        );
    }
}
