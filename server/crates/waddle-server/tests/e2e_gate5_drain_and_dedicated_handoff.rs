//! T4-S12: Release Evidence Gate 1/5 (M6) — `WADDLE_DRAIN_TIMEOUT_SECS` pod
//! drain plus shared-to-dedicated cell handoff via `TursoPlacementLeaseStore`
//! and `BrokerReplicaFence`, aborting old shared-pool inference streams while
//! admitting fresh inference on the dedicated cell holder cleanly.

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
use waddle_broker_core::admission::{BudgetLimits, FenceKey, FenceKind};
use waddle_broker_core::boot_clock::SystemBootClock;
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{AccountId, CellRole, SpiffeId, TenantId};
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    AuthzTuple, PermissionBackend,
};
use waddle_broker_core::placement::{PlacementLeaseStore, PlacementMode};
use waddle_ecdysis::GracefulShutdown;
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceError, HostInferenceErrorCode, HostInferenceMessage,
    HostInferenceRequest, InferenceCredentialRef, StreamItem,
};
use waddle_extensions::{PluginId, WaddleId};
use waddle_server::server::extension_inference_host::{
    BrokerCredentialResolver, BrokerExtensionInferenceHost, BrokerExtensionInferenceHostConfig,
};
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;
use waddle_tenancy::quiesce::drain_timeout_from_env;

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
async fn gate5_graceful_drain_and_shared_to_dedicated_cell_handoff() {
    let budget = drain_timeout_from_env();
    let shutdown = GracefulShutdown::new(budget);
    let stop_token = shutdown.stop_token();
    shutdown.trigger_stop();
    tokio::time::timeout(Duration::from_secs(2), stop_token.cancelled())
        .await
        .expect("graceful drain token cancelled promptly");

    let tls = generate_test_tls_material("provider.acme.test");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);
    let release_shared = Arc::new(Notify::new());
    let release_srv = Arc::clone(&release_shared);

    let srv = tokio::spawn(async move {
        // Stream 1 on shared cell: send initial chunk then hold open until handoff aborts it.
        let (tcp1, _) = listener.accept().await.expect("accept 1");
        let mut s1 = acceptor.accept(tcp1).await.expect("tls 1");
        let _ = read_http_request(&mut s1).await;
        let c1 = "data: {\"choices\":[{\"delta\":{\"content\":\"shared-part\"}}]}\n\n";
        let h1 = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n",
            c1.len(),
            c1
        );
        s1.write_all(h1.as_bytes()).await.expect("write shared");
        s1.flush().await.expect("flush shared");
        release_srv.notified().await;

        // Stream 2 on dedicated cell completes end-to-end.
        let (tcp2, _) = listener.accept().await.expect("accept 2");
        let mut s2 = acceptor.accept(tcp2).await.expect("tls 2");
        let _ = read_http_request(&mut s2).await;
        let b2 = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"dedicated-ok\"}}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
            "data: [DONE]\n\n"
        );
        let r2 = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            b2.len(),
            b2
        );
        s2.write_all(r2.as_bytes()).await.expect("write dedicated");
        s2.shutdown().await.expect("shutdown dedicated");
    });

    let dir = TempDir::new().expect("tempdir");
    let tenant_id = TenantId::parse("acme.example").expect("tenant");
    let account_id = AccountId::new_v4();

    let lease_store = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("leases.db"))
            .await
            .expect("lease store"),
    );
    let spiffe_shared =
        SpiffeId::parse("spiffe://acme.example/ns/cells/sa/shared-pool").expect("spiffe shared");
    let binding_shared = [0x11u8; 32];
    let kp_shared = HolderKeypair::generate().expect("kp shared");
    let holder_shared = kp_shared.sign_channel_binding(&spiffe_shared, &binding_shared);

    let spiffe_dedicated =
        SpiffeId::parse("spiffe://acme.example/ns/cells/sa/dedicated-acme").expect("spiffe ded");
    let binding_dedicated = [0x22u8; 32];
    let kp_dedicated = HolderKeypair::generate().expect("kp dedicated");
    let holder_dedicated = kp_dedicated.sign_channel_binding(&spiffe_dedicated, &binding_dedicated);

    let shared_role = CellRole::parse("shared-pool").expect("shared cell role");
    let dedicated_role = CellRole::parse("dedicated-acme").expect("dedicated cell role");
    let shared_lease = lease_store
        .claim(
            &tenant_id,
            &shared_role,
            PlacementMode::SharedPool,
            &holder_shared,
            30_000,
        )
        .await
        .expect("claim shared lease");

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
            provider: "openai".to_owned(),
            nonce: format!("nonce-{}", Uuid::new_v4()),
            payload_digest_sha256: [8u8; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(b"sk-test-handoff-secret".to_vec()).expect("secret"),
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
    let admission = Arc::new(BrokerAdmissionService::new(
        "broker-gate5-handoff",
        Arc::clone(&ledger),
        perms,
        Arc::new(DeviceKeyRegistry::new()),
        Arc::clone(&replica_fence),
        TraceRecorder::new(),
    ));
    let resolver: Arc<dyn BrokerCredentialResolver> = Arc::new(StubResolver {
        account_id,
        credential_id: cred.credential_id,
    });
    let budget = BudgetLimits {
        max_input_tokens: 4_096,
        max_output_tokens: 1_024,
        max_spend_microcents: 50_000,
        max_steps: 4,
    };

    let shared_host = BrokerExtensionInferenceHost::new(
        BrokerExtensionInferenceHostConfig {
            trust_domain: "acme.example".to_owned(),
            holder: holder_shared.clone(),
            peer_spiffe_id: spiffe_shared,
            channel_binding: binding_shared,
            default_budget: budget,
            default_deployment: "prod".to_owned(),
            default_reserved_spend_microcents: 100,
        },
        Arc::clone(&admission),
        Arc::clone(&egress),
        Arc::clone(&resolver),
    );
    let dedicated_host = BrokerExtensionInferenceHost::new(
        BrokerExtensionInferenceHostConfig {
            trust_domain: "acme.example".to_owned(),
            holder: holder_dedicated.clone(),
            peer_spiffe_id: spiffe_dedicated,
            channel_binding: binding_dedicated,
            default_budget: budget,
            default_deployment: "prod".to_owned(),
            default_reserved_spend_microcents: 100,
        },
        Arc::clone(&admission),
        egress,
        resolver,
    );

    let ctx = InvocationContext {
        tenant_id: Some(tenant_id.clone()),
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
            content: "handoff test".to_owned(),
        }],
        max_output_tokens: Some(64),
    };

    let mut shared_stream = shared_host
        .start_stream(&ctx, req.clone())
        .await
        .expect("shared start_stream");
    match tokio::time::timeout(Duration::from_secs(5), shared_stream.rx.recv())
        .await
        .expect("shared chunk timeout")
    {
        Some(StreamItem::Chunk(c)) => {
            assert!(String::from_utf8_lossy(&c.delta).contains("shared-part"));
        }
        other => panic!("expected shared-part chunk, got {other:?}"),
    }

    // Shared-to-dedicated handoff: close placement fence across transfer, then reopen under dedicated lease!
    let lease_store_c = Arc::clone(&lease_store);
    let tenant_c = tenant_id.clone();
    let hs = holder_shared.clone();
    let hd = holder_dedicated.clone();
    let placement_key = FenceKey {
        tenant_id: tenant_id.clone(),
        kind: FenceKind::Placement,
    };
    admission
        .restrict_fence(&placement_key, &[], true, move || async move {
            lease_store_c
                .release(&tenant_c, &hs, shared_lease.generation)
                .await
                .map_err(|e| {
                    waddle_broker_core::permission::PermissionBackendError::Backend(e.to_string())
                })?;
            let next = lease_store_c
                .claim(
                    &tenant_c,
                    &dedicated_role,
                    PlacementMode::DedicatedCell,
                    &hd,
                    30_000,
                )
                .await
                .map_err(|e| {
                    waddle_broker_core::permission::PermissionBackendError::Backend(e.to_string())
                })?;
            assert!(next.generation > shared_lease.generation);
            assert_eq!(next.mode, PlacementMode::DedicatedCell);
            Ok(())
        })
        .await
        .expect("restrict and transfer lease");
    replica_fence.tick_once().await.expect("tick");

    let shared_abort = tokio::time::timeout(Duration::from_secs(5), shared_stream.rx.recv())
        .await
        .expect("shared abort timeout")
        .expect("shared abort item");
    match shared_abort {
        StreamItem::Error(err) => assert_eq!(err.code, HostInferenceErrorCode::Canceled),
        other => panic!("expected Canceled on old shared stream, got {other:?}"),
    }
    release_shared.notify_one();

    // Dedicated holder starts fresh stream cleanly once dedicated lease is active.
    let mut dedicated_stream = dedicated_host
        .start_stream(&ctx, req)
        .await
        .expect("dedicated host start_stream");
    let mut dedicated_text = String::new();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(5), dedicated_stream.rx.recv())
        .await
        .expect("dedicated chunk timeout")
    {
        match item {
            StreamItem::Chunk(c) => dedicated_text.push_str(&String::from_utf8_lossy(&c.delta)),
            StreamItem::EndOfStream(usage) => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 5);
                break;
            }
            StreamItem::Error(err) => panic!("unexpected dedicated stream error: {err:?}"),
        }
    }
    assert!(dedicated_text.contains("dedicated-ok"));
    srv.await.expect("server");
}
