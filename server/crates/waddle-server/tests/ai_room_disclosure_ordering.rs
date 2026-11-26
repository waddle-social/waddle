//! T1-F20-03: Server-owned room disclosure for `/ai` must happen strictly
//! AFTER broker admission succeeds and strictly BEFORE inference egress
//! sends any bytes to the provider. Denied admissions and failed disclosures
//! must never emit provider egress traffic.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use tokio::sync::Mutex;
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
    HostInferenceRequest, InferenceCredentialRef, StreamItem,
};
use waddle_extensions::{PluginId, WaddleId};
use waddle_server::server::extension_inference_host::{
    BrokerCredentialResolver, BrokerExtensionInferenceHost, BrokerExtensionInferenceHostConfig,
    RoomDisclosureEmitter, RoomDisclosureEvent,
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
            "member invocation denied in test",
        ))
    }
}

struct RecordingDisclosureEmitter {
    timeline: Arc<Mutex<Vec<String>>>,
    events: Arc<Mutex<Vec<RoomDisclosureEvent>>>,
    ledger: Arc<TursoAdmissionLedgerStore>,
    fail_disclosure: Arc<AtomicBool>,
}

#[async_trait]
impl RoomDisclosureEmitter for RecordingDisclosureEmitter {
    async fn emit_room_disclosure(
        &self,
        event: &RoomDisclosureEvent,
    ) -> Result<(), HostInferenceError> {
        // Prove admission has ALREADY committed the attempt row before room disclosure fires!
        let attempt = self
            .ledger
            .get_attempt(event.attempt_id)
            .await
            .expect("lookup attempt")
            .expect("attempt must already be recorded by admission before room disclosure");
        assert_eq!(attempt.status, AttemptStatus::Admitted);
        self.events.lock().await.push(event.clone());
        self.timeline
            .lock()
            .await
            .push("room_disclosure".to_owned());
        if self.fail_disclosure.load(Ordering::SeqCst) {
            return Err(HostInferenceError::new(
                HostInferenceErrorCode::TemporaryFailure,
                "disclosure delivery failed before egress",
            ));
        }
        Ok(())
    }
}

async fn setup_disclosure_host(
    server_addr: SocketAddr,
    ca_pem: Vec<u8>,
    grant_tuple: bool,
    fail_disclosure: Arc<AtomicBool>,
) -> (
    TempDir,
    BrokerExtensionInferenceHost<TursoAdmissionLedgerStore>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<RoomDisclosureEvent>>>,
    TenantId,
) {
    let dir = TempDir::new().expect("tempdir");
    let tenant_id = TenantId::parse("acme.example").expect("tenant");
    let account_id = AccountId::new_v4();
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
            plaintext: SecretBytes::new(b"sk-test-disclosure-secret".to_vec()).expect("secret"),
        })
        .await
        .expect("enroll cred");

    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    perms
        .provision_tenant_store(&tenant_id)
        .await
        .expect("provision store");
    if grant_tuple {
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
    }

    let plugin_id = PluginId::try_new("ai-chatbot").expect("plugin id");
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
        "broker-disclosure-test",
        Arc::clone(&ledger),
        perms,
        Arc::new(DeviceKeyRegistry::new()),
        replica_fence,
        TraceRecorder::new(),
    ));

    let timeline = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(RecordingDisclosureEmitter {
        timeline: Arc::clone(&timeline),
        events: Arc::clone(&events),
        ledger: Arc::clone(&ledger),
        fail_disclosure,
    });

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
            credential_id: cred_receipt.credential_id,
        }),
    )
    .with_disclosure_emitter(emitter);

    (dir, host, timeline, events, tenant_id)
}

#[tokio::test]
async fn room_disclosure_fires_post_admission_pre_egress_and_skips_denied_calls() {
    let tls = generate_test_tls_material("provider.acme.test");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);
    let egress_hits = Arc::new(AtomicUsize::new(0));
    let fail_disclosure = Arc::new(AtomicBool::new(false));

    let (_dir, host, timeline, events, tenant_id) =
        setup_disclosure_host(addr, tls.ca_pem.clone(), true, Arc::clone(&fail_disclosure)).await;

    let timeline_srv = Arc::clone(&timeline);
    let egress_hits_srv = Arc::clone(&egress_hits);
    let srv = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut tls_stream = acceptor.accept(tcp).await.expect("tls accept");
        let _req = read_http_request(&mut tls_stream).await;
        egress_hits_srv.fetch_add(1, Ordering::SeqCst);
        timeline_srv.lock().await.push("egress_received".to_owned());
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
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

    let room: BareJid = "general@muc.acme.example".parse().expect("room");
    let requester: BareJid = "alice@acme.example".parse().expect("requester");
    let context = InvocationContext {
        tenant_id: Some(tenant_id.clone()),
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        plugin_id: PluginId::try_new("ai-chatbot").expect("plugin"),
        requester: Some(requester.clone()),
        source_room: Some(room.clone()),
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
            content: "summarize thread".to_owned(),
        }],
        max_output_tokens: Some(64),
    };

    let mut opened = host
        .start_stream(&context, req.clone())
        .await
        .expect("admitted room /ai must succeed");
    while let Some(item) = opened.rx.recv().await {
        if matches!(item, StreamItem::EndOfStream(_)) {
            break;
        }
    }
    srv.await.expect("server task");

    assert_eq!(
        timeline.lock().await.as_slice(),
        &["room_disclosure", "egress_received"]
    );
    assert_eq!(egress_hits.load(Ordering::SeqCst), 1);
    let captured = events.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].room_jid, room);
    assert_eq!(captured[0].requester, Some(requester));
    drop(captured);

    // When disclosure itself fails, egress must NOT receive any request.
    fail_disclosure.store(true, Ordering::SeqCst);
    timeline.lock().await.clear();
    let disclosure_err = host
        .start_stream(&context, req.clone())
        .await
        .err()
        .expect("failed disclosure must fail closed before egress");
    assert_eq!(
        disclosure_err.code,
        HostInferenceErrorCode::TemporaryFailure
    );
    assert_eq!(egress_hits.load(Ordering::SeqCst), 1);
    assert_eq!(timeline.lock().await.as_slice(), &["room_disclosure"]);

    // Denied admission must NOT trigger disclosure or egress.
    let (_dir2, denied_host, denied_timeline, denied_events, denied_tenant) =
        setup_disclosure_host(addr, tls.ca_pem, false, Arc::new(AtomicBool::new(false))).await;
    let denied_context = InvocationContext {
        tenant_id: Some(denied_tenant),
        ..context
    };
    let denied = denied_host
        .start_stream(&denied_context, req)
        .await
        .err()
        .expect("missing permission tuple must deny invocation");
    assert_eq!(denied.code, HostInferenceErrorCode::Denied);
    assert!(denied_events.lock().await.is_empty());
    assert!(denied_timeline.lock().await.is_empty());
    assert_eq!(egress_hits.load(Ordering::SeqCst), 1);
}
