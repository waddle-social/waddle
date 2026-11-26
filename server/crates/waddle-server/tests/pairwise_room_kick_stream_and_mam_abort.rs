//! T3-06: Pairwise (F14 + F15 + F20) — Mid-stream room kick closes the room
//! fence (`BrokerReplicaFence`), aborts the active brokered inference stream,
//! and removes member channel access so `ExtensionHostAdapter::query_mam` returns
//! `Denied`.

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
use tokio::sync::Notify;
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
use waddle_broker_core::admission::{BudgetLimits, FenceKey, FenceKind, RoomScopeKey};
use waddle_broker_core::boot_clock::SystemBootClock;
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{AccountId, SpiffeId, TenantId};
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    AuthzTuple, PermissionBackend,
};
use waddle_extensions::config::ExtensionConfig;
use waddle_extensions::host_tools::{
    ExtensionHostTools, InvocationContext, InvocationKind, MamQuery, MamTarget,
};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceError, HostInferenceErrorCode, HostInferenceMessage,
    HostInferenceRequest, InferenceCredentialRef, StreamItem,
};
use waddle_extensions::{ExtensionManager, PluginId, WaddleId};
use waddle_server::auth::{NativeUserStore, RegisterRequest};
use waddle_server::permissions::{
    DeleteTuple, Object, ObjectType, Relation, Subject, Tuple, WriteTuple,
};
use waddle_server::server::build_websocket_state_for_test;
use waddle_server::server::extension_host_adapter::ExtensionHostAdapter;
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
            "unused in tenant stream test",
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
async fn pairwise_room_kick_aborts_active_stream_and_revokes_mam_access() {
    let tls = generate_test_tls_material("provider.acme.test");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);
    let release_server = Arc::new(Notify::new());
    let release_srv = Arc::clone(&release_server);

    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut tls_stream = acceptor.accept(tcp).await.expect("tls accept");
        let _req = read_http_request(&mut tls_stream).await;
        let first_chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"part-1\"}}]}\n\n";
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n",
            first_chunk.len(),
            first_chunk
        );
        tls_stream
            .write_all(headers.as_bytes())
            .await
            .expect("write chunk 1");
        tls_stream.flush().await.expect("flush chunk 1");
        release_srv.notified().await;
    });

    // 1. Setup brokered inference host + room fence.
    let dir = TempDir::new().expect("tempdir");
    let tenant_id = TenantId::parse("example.com").expect("tenant");
    let account_id = AccountId::new_v4();
    let spiffe_id =
        SpiffeId::parse("spiffe://example.com/ns/cells/sa/shared-pool").expect("spiffe");
    let keypair = HolderKeypair::generate().expect("keypair");
    let channel_binding = [0x21u8; 32];
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
    let hierarchy = Arc::new(KeyHierarchyService::new("example.com", kms));
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
            payload_digest_sha256: [3u8; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(b"sk-test-room-kick".to_vec()).expect("secret"),
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
                expected_trust_domain: Some("example.com".to_owned()),
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
        addr,
    ));
    let admission = Arc::new(BrokerAdmissionService::new(
        "broker-room-kick",
        Arc::clone(&ledger),
        perms,
        Arc::new(DeviceKeyRegistry::new()),
        Arc::clone(&replica_fence),
        TraceRecorder::new(),
    ));
    let inf_host = BrokerExtensionInferenceHost::new(
        BrokerExtensionInferenceHostConfig {
            trust_domain: "example.com".to_owned(),
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
        Arc::clone(&admission),
        egress,
        Arc::new(StubResolver {
            account_id,
            credential_id: cred.credential_id,
        }),
    );

    // 2. Setup WebSocketState + ExtensionHostAdapter for MAM re-authz verification.
    let ext_mgr = Arc::new(
        ExtensionManager::from_config(ExtensionConfig {
            enabled: false,
            cache_dir: dir.path().join("ext").display().to_string(),
            modules: Vec::new(),
        })
        .await
        .expect("ext mgr"),
    );
    let (state, _shutdown) = build_websocket_state_for_test(ext_mgr, "example.com", 5).await;
    let alice_bare: BareJid = "alice@example.com".parse().expect("alice");
    NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone())
        .register(RegisterRequest {
            username: "alice".to_owned(),
            domain: "example.com".to_owned(),
            password: "pw".to_owned(),
            email: None,
        })
        .await
        .expect("seed alice");
    let room: BareJid = "room-a@muc.example.com".parse().expect("room");
    let room_actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
            room_jid: room.clone(),
            waddle_id: "example.com".to_owned(),
            channel_id: "room-a".to_owned(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("create room");
    room_actor
        .ask(waddle_xmpp::muc::room_actor::ChangeAffiliation {
            jid: alice_bare.clone(),
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("grant member");
    let member_tuple = Tuple::new(
        Object::new(ObjectType::Channel, "room-a"),
        Relation::new("member"),
        Subject::user(alice_bare.as_str()),
    );
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: member_tuple.clone(),
        })
        .await
        .expect("write channel tuple");
    let ext_host = ExtensionHostAdapter::new(Arc::clone(&state));
    let context = InvocationContext {
        tenant_id: Some(tenant_id.clone()),
        waddle_id: WaddleId::try_new("example.com").expect("waddle"),
        plugin_id,
        requester: Some(alice_bare.clone()),
        source_room: Some(room.clone()),
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    };
    let mam_query = MamQuery {
        target: MamTarget::Room(room.clone()),
        start: None,
        end: None,
        thread_id: None,
        sender: None,
        text: None,
        max_results: 10,
    };
    ext_host
        .query_mam(&context, mam_query.clone())
        .await
        .expect("MAM query must succeed before room kick");

    // 3. Start streaming inference, consume chunk 0, then kick Alice + restrict room fence mid-round!
    let mut stream = inf_host
        .start_stream(
            &context,
            HostInferenceRequest {
                credential: InferenceCredentialRef::Tenant("default".to_owned()),
                provider: "openai".to_owned(),
                deployment: "prod".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                messages: vec![HostInferenceMessage {
                    role: "user".to_owned(),
                    content: "summarize room".to_owned(),
                }],
                max_output_tokens: Some(128),
            },
        )
        .await
        .expect("start_stream");
    match tokio::time::timeout(Duration::from_secs(5), stream.rx.recv())
        .await
        .expect("first chunk timeout")
    {
        Some(StreamItem::Chunk(c)) => {
            assert!(String::from_utf8_lossy(&c.delta).contains("part-1"));
        }
        other => panic!("expected first chunk, got {other:?}"),
    }

    // Mid-round kick: delete member permission + advance room fence via restrict_fence.
    state
        .deps
        .app_state
        .permission_actor
        .ask(DeleteTuple {
            tuple: member_tuple,
        })
        .await
        .expect("revoke channel membership");
    admission
        .restrict_fence(
            &FenceKey {
                tenant_id,
                kind: FenceKind::RoomOrSpace(RoomScopeKey {
                    scope_id: room.to_string(),
                }),
            },
            &[],
            true,
            || async { Ok(()) },
        )
        .await
        .expect("restrict room fence on kick");
    replica_fence.tick_once().await.expect("tick after kick");

    // Stream must abort immediately with Canceled.
    let cancel_item = tokio::time::timeout(Duration::from_secs(5), stream.rx.recv())
        .await
        .expect("cancel timeout")
        .expect("cancel item");
    match cancel_item {
        StreamItem::Error(err) => assert_eq!(err.code, HostInferenceErrorCode::Canceled),
        other => panic!("expected StreamItem::Error(Canceled), got {other:?}"),
    }

    // Next MAM tool round for kicked user fails closed.
    let mam_after_kick = ext_host.query_mam(&context, mam_query).await;
    assert!(
        mam_after_kick.is_err(),
        "kicked user MAM query must fail closed"
    );
    release_server.notify_one();
}
