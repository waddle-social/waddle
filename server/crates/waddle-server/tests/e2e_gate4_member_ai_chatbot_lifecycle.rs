//! T4-S07: Release Evidence Gate 3/4 (M6) — Member-funded `/ai` full E2E
//! lifecycle: output-mode enforcement, device-signed P-256 member invocation,
//! post-admission pre-egress room disclosure, loopback TLS streaming inference,
//! and ledger usage reconciliation.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use jid::BareJid;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::rand_core::OsRng;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;
use waddle_broker::admission::invocation::{PayerSelection, SignedInferenceInvocation};
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
use waddle_broker::idp_proof::{
    build_device_enroll_pop_transcript, p256_jwk_thumbprint, AuthProvenance, DeviceKeyRegistry,
    EnrollDeviceKeyInput, VerifiedIdpIntent,
};
use waddle_broker::key_hierarchy::KeyHierarchyService;
use waddle_broker::kms::local::LocalKmsProvider;
use waddle_broker::kms::KmsProvider;
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::secret::SecretBytes;
use waddle_broker_core::admission::{AttemptId, BudgetLimits, RoomScopeKey};
use waddle_broker_core::boot_clock::SystemBootClock;
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{AccountId, SpiffeId, TenantId};
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    AuthzTuple, PermissionBackend,
};
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceError, HostInferenceMessage, HostInferenceRequest,
    InferenceCredentialRef, StreamItem,
};
use waddle_extensions::{
    DataFormValue, FormFieldValue, PluginId, RoomJid as ExtensionRoomJid, UiActionId, WaddleId,
};
use waddle_server::server::extension_inference_host::{
    BrokerCredentialResolver, BrokerExtensionInferenceHost, BrokerExtensionInferenceHostConfig,
    RoomDisclosureEmitter, RoomDisclosureEvent,
};
use waddle_server::server::validate_room_ai_output_mode;

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

struct MemberSignedResolver {
    trust_domain: String,
    account_id: AccountId,
    credential_id: Uuid,
    device_key_id: Uuid,
    signing_key: SigningKey,
    budget: BudgetLimits,
}

#[async_trait]
impl BrokerCredentialResolver for MemberSignedResolver {
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
        unreachable!("member-funded lifecycle selects InvokingUser, not Tenant")
    }

    async fn resolve_member_invocation(
        &self,
        tenant_id: &TenantId,
        context: &InvocationContext,
        provider: &str,
    ) -> Result<(AccountId, Uuid, SignedInferenceInvocation), HostInferenceError> {
        let mut ext_digest = [0u8; 32];
        ext_digest.copy_from_slice(&Sha256::digest(context.plugin_id.as_str().as_bytes()));
        let mut cmd_digest = [0u8; 32];
        cmd_digest.copy_from_slice(&Sha256::digest(
            format!(
                "{}:{}:default:openrouter/free",
                context.plugin_id.as_str(),
                provider
            )
            .as_bytes(),
        ));
        let room_scope = RoomScopeKey {
            scope_id: context
                .source_room
                .as_ref()
                .map(|r| r.as_str().to_owned())
                .unwrap_or_else(|| format!("waddle:{}", context.waddle_id.as_str())),
        };
        let mut nonce = [0u8; 32];
        nonce[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        nonce[16..].copy_from_slice(Uuid::new_v4().as_bytes());

        let mut signed = SignedInferenceInvocation {
            trust_domain: self.trust_domain.clone(),
            tenant_id: tenant_id.clone(),
            account_id: self.account_id,
            device_key_id: self.device_key_id,
            extension_id: context.plugin_id.as_str().to_owned(),
            extension_digest_sha256: ext_digest,
            room_scope,
            provider: provider.to_owned(),
            payer: PayerSelection::Member {
                account_id: self.account_id,
                credential_id: self.credential_id,
            },
            command_digest_sha256: cmd_digest,
            nonce,
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            budget: self.budget,
            signature: Vec::new(),
        };
        let sig: Signature = self.signing_key.sign(&signed.canonical_transcript());
        signed.signature = sig.to_bytes().to_vec();
        Ok((self.account_id, self.credential_id, signed))
    }
}

struct Gate4DisclosureEmitter {
    timeline: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl RoomDisclosureEmitter for Gate4DisclosureEmitter {
    async fn emit_room_disclosure(
        &self,
        _event: &RoomDisclosureEvent,
    ) -> Result<(), HostInferenceError> {
        self.timeline.lock().await.push("disclosure".to_owned());
        Ok(())
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
async fn gate4_member_ai_chatbot_full_e2e_lifecycle() {
    let ext_room = ExtensionRoomJid::try_new("general@muc.acme.example").expect("room");
    let field = |name: &str, value: &str| FormFieldValue {
        name: UiActionId::try_new(name).expect("name"),
        values: vec![DataFormValue::new(value)],
    };
    assert!(validate_room_ai_output_mode(
        "urn:waddle:extension:1:ai-chatbot",
        Some(&ext_room),
        &[field("prompt", "hello"), field("output", "private")],
    )
    .is_err());
    validate_room_ai_output_mode(
        "urn:waddle:extension:1:ai-chatbot",
        Some(&ext_room),
        &[field("prompt", "hello"), field("output", "channel")],
    )
    .expect("channel output allowed");

    let tls = generate_test_tls_material("openrouter.acme.test");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);
    let timeline = Arc::new(Mutex::new(Vec::new()));
    let timeline_srv = Arc::clone(&timeline);

    let srv = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut tls_stream = acceptor.accept(tcp).await.expect("tls accept");
        let req = read_http_request(&mut tls_stream).await;
        assert!(req.contains("sk-member-byok-secret"));
        timeline_srv.lock().await.push("egress".to_owned());
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Member answer\"}}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":21,\"completion_tokens\":9}}\n\n",
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
    let account_id = AccountId::new_v4();
    let spiffe_id =
        SpiffeId::parse("spiffe://acme.example/ns/cells/sa/shared-pool").expect("spiffe");
    let keypair = HolderKeypair::generate().expect("keypair");
    let channel_binding = [0x12u8; 32];
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
            account_id: Some(account_id),
            provider: "openrouter".to_owned(),
            nonce: format!("nonce-{}", Uuid::new_v4()),
            payload_digest_sha256: [1u8; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(b"sk-member-byok-secret".to_vec()).expect("secret"),
        })
        .await
        .expect("enroll member key");

    let device_keys = Arc::new(DeviceKeyRegistry::new());
    let sk = SigningKey::random(&mut OsRng);
    let pk = sk
        .verifying_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    let jkt = p256_jwk_thumbprint(&pk).expect("jkt");
    let dev_id = Uuid::new_v4();
    let now = Utc::now().timestamp_millis();
    let exp = now + 60_000;
    let transcript = build_device_enroll_pop_transcript(
        "acme.example",
        &tenant_id,
        account_id,
        dev_id,
        "pop-1",
        exp,
    );
    let sig: Signature = sk.sign(&transcript);
    device_keys
        .enroll_device_key(EnrollDeviceKeyInput {
            trust_domain: "acme.example",
            provenance: AuthProvenance::ExternalIdp,
            idp_intent: &VerifiedIdpIntent {
                account_id,
                tenant_id: tenant_id.clone(),
                device_jkt: Some(jkt),
                is_tenant_admin: false,
                proof_id: "idp-member".to_owned(),
            },
            device_key_id: dev_id,
            public_key_sec1: pk,
            nonce: "pop-1",
            expires_at_ms: exp,
            now_ms: now,
            pop_signature: &sig.to_bytes(),
        })
        .expect("enroll device key");

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
                    object_type: AuthzObjectType::UserCredential,
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
        .expect("member credential tuple");

    let plugin_id = PluginId::try_new("ai-chatbot").expect("plugin");
    let mut ext_digest = [0u8; 32];
    ext_digest.copy_from_slice(&Sha256::digest(plugin_id.as_str().as_bytes()));
    let mut profile_registry = EgressProfileRegistry::new();
    profile_registry
        .register_with_policy(
            ProviderProfile {
                provider: "openrouter".to_owned(),
                deployment: "default".to_owned(),
                model: "openrouter/free".to_owned(),
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
    let budget = BudgetLimits {
        max_input_tokens: 4_096,
        max_output_tokens: 1_024,
        max_spend_microcents: 50_000,
        max_steps: 4,
    };
    let admission = Arc::new(BrokerAdmissionService::new(
        "broker-gate4-member",
        Arc::clone(&ledger),
        perms,
        device_keys,
        replica_fence,
        TraceRecorder::new(),
    ));
    let host = BrokerExtensionInferenceHost::new(
        BrokerExtensionInferenceHostConfig {
            trust_domain: "acme.example".to_owned(),
            holder,
            peer_spiffe_id: spiffe_id,
            channel_binding,
            default_budget: budget,
            default_deployment: "default".to_owned(),
            default_reserved_spend_microcents: 120,
        },
        admission,
        egress,
        Arc::new(MemberSignedResolver {
            trust_domain: "acme.example".to_owned(),
            account_id,
            credential_id: cred.credential_id,
            device_key_id: dev_id,
            signing_key: sk,
            budget,
        }),
    )
    .with_disclosure_emitter(Arc::new(Gate4DisclosureEmitter {
        timeline: Arc::clone(&timeline),
    }));

    let mut opened = host
        .start_stream(
            &InvocationContext {
                tenant_id: Some(tenant_id),
                waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
                plugin_id,
                requester: Some("alice@acme.example".parse().expect("alice")),
                source_room: Some("general@muc.acme.example".parse().expect("room")),
                kind: InvocationKind::Command,
                provider_room_grants: Vec::new(),
            },
            HostInferenceRequest {
                credential: InferenceCredentialRef::InvokingUser("openrouter".to_owned()),
                provider: "openrouter".to_owned(),
                deployment: "default".to_owned(),
                model: "openrouter/free".to_owned(),
                messages: vec![HostInferenceMessage {
                    role: "user".to_owned(),
                    content: "summarize release".to_owned(),
                }],
                max_output_tokens: Some(128),
            },
        )
        .await
        .expect("start_stream");

    let mut usage = None;
    let mut chunks = String::new();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(5), opened.rx.recv())
        .await
        .expect("stream timeout")
    {
        match item {
            StreamItem::Chunk(c) => chunks.push_str(&String::from_utf8_lossy(&c.delta)),
            StreamItem::EndOfStream(u) => {
                usage = Some(u);
                break;
            }
            StreamItem::Error(err) => panic!("unexpected stream error: {err:?}"),
        }
    }
    srv.await.expect("server");
    assert!(chunks.contains("Member answer"));
    let u = usage.expect("usage");
    assert_eq!(u.input_tokens, 21);
    assert_eq!(u.output_tokens, 9);
    assert_eq!(timeline.lock().await.as_slice(), &["disclosure", "egress"]);

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
    assert_eq!(rec.input_tokens, 21);
    assert_eq!(rec.output_tokens, 9);
}
