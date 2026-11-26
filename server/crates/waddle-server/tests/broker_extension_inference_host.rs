//! Integration tests exercising `BrokerExtensionInferenceHost` directly against
//! `BrokerAdmissionService`, `BrokerEgressClient`, `TursoAdmissionLedgerStore`,
//! holder SPIFFE/tls-exporter verification, canonical account resolution, and
//! `ExtensionManager::from_config_for_tenant_with_hosts`.

#[path = "broker_test_support/admission_db.rs"]
mod admission_db;
#[path = "broker_test_support/broker_host.rs"]
mod broker_host;
#[path = "broker_test_support/tls_provider.rs"]
mod tls_provider;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use admission_db::load_attempt_ids;
use async_trait::async_trait;
use broker_host::{build_broker_test_env, BrokerTestConfig};
use jid::BareJid;
use tempfile::TempDir;
use tls_provider::{read_http_request, LoopbackTlsServer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;
use waddle_broker::admission::service::TraceRecorder;
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::{AdmissionQueryStore, AttemptStatus};
use waddle_broker::idp_proof::{AuthProvenance, DeviceKeyRegistry};
use waddle_broker_core::identity::{AccountId, SpiffeId, TenantId};
use waddle_broker_core::permission::{AuthzObjectType, AuthzSubject, AuthzSubjectType};
use waddle_broker_core::quotas::{TenantResourceController, TenantResourceQuotas};
use waddle_extensions::config::{ExtensionConfig, ExtensionModuleConfig, RuntimeLimits};
use waddle_extensions::host_tools::{DenyingExtensionHostTools, InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceError, HostInferenceErrorCode, HostInferenceMessage,
    HostInferenceRequest, InferenceCredentialRef, StreamItem,
};
use waddle_extensions::{ExtensionCapability, ExtensionManager, PluginId, WaddleId};
use waddle_server::server::extension_inference_host::{
    BrokerCredentialResolver, BrokerExtensionInferenceHost,
};

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
    dir: TempDir,
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
    let account_id = AccountId::new_v4();
    let requester: BareJid = "alice@acme.example".parse().expect("requester");
    let (env, ledger, _fence) = build_broker_test_env(BrokerTestConfig {
        data_dir: dir.path(),
        trust_domain: "acme.example",
        broker_id: "broker-test",
        spiffe_id: "spiffe://acme.example/ns/cells/sa/shared-pool",
        channel_binding: [0x42u8; 32],
        provider: "openai",
        deployment: "prod",
        model: "gpt-4o-mini",
        base_origin: "https://provider.acme.test",
        operation_path: "/v1/chat/completions",
        plugin_id: "ai-chatbot",
        credential_secret: b"sk-test-broker-secret",
        credential_account_id: None,
        permission_grant: Some((
            AuthzObjectType::TenantCredential,
            AuthzSubject {
                subject_type: AuthzSubjectType::Account,
                id: account_id.to_string(),
                relation: None,
            },
        )),
        device_keys: Arc::new(DeviceKeyRegistry::new()),
        trace: TraceRecorder::new(),
        server_addr,
        ca_pem,
    })
    .await;

    let resolver = Arc::new(StubCredentialResolver {
        expected_requester: requester.clone(),
        account_id,
        credential_id: env.credential_id,
        resolved_requesters: Mutex::new(Vec::new()),
    });

    let host = if binding_override.is_some() || peer_spiffe_override.is_some() {
        let mut host_cfg = env.build_host_config(250);
        if let Some(binding) = binding_override {
            host_cfg.channel_binding = binding;
        }
        if let Some(peer_spiffe) = peer_spiffe_override {
            host_cfg.peer_spiffe_id = peer_spiffe;
        }
        BrokerExtensionInferenceHost::new(
            host_cfg,
            Arc::clone(&env.admission),
            Arc::clone(&env.egress),
            Arc::clone(&resolver) as Arc<dyn BrokerCredentialResolver>,
        )
    } else {
        env.build_host(
            Arc::clone(&resolver) as Arc<dyn BrokerCredentialResolver>,
            250,
        )
    };

    TestHostEnv {
        dir,
        tenant_id: env.tenant_id,
        account_id,
        requester,
        spiffe_id: env.spiffe_id,
        channel_binding: env.channel_binding,
        ledger,
        resolver,
        host,
        plugin_id: env.plugin_id,
    }
}

#[tokio::test]
async fn broker_extension_inference_host_streams_resolves_account_and_reconciles_usage() {
    let tls = LoopbackTlsServer::bind("provider.acme.test").await;
    let listener = tls.listener;
    let acceptor = tls.acceptor;

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

    let env = setup_test_host(tls.addr, tls.ca_pem, None, None).await;
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
    let attempt_id = load_attempt_ids(env.dir.path()).await[0];
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
    let tls = LoopbackTlsServer::bind("provider.acme.test").await;

    // 1. Channel binding mismatch -> HolderInstance verification fails closed.
    let bad_binding_env =
        setup_test_host(tls.addr, tls.ca_pem.clone(), Some([0x99u8; 32]), None).await;
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
    let bad_spiffe_env =
        setup_test_host(tls.addr, tls.ca_pem.clone(), None, Some(wrong_spiffe)).await;
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
    let valid_env = setup_test_host(tls.addr, tls.ca_pem, None, None).await;
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
    let tls = LoopbackTlsServer::bind("provider.acme.test").await;
    let listener = tls.listener;
    let acceptor = tls.acceptor;
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

    let env = setup_test_host(tls.addr, tls.ca_pem, None, None).await;
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
    let attempt_id = load_attempt_ids(env.dir.path()).await[0];
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
