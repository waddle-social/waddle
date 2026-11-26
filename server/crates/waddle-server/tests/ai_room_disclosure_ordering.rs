//! T1-F20-03: Server-owned room disclosure for `/ai` must happen strictly
//! AFTER broker admission succeeds and strictly BEFORE inference egress
//! sends any bytes to the provider. Denied admissions and failed disclosures
//! must never emit provider egress traffic.

#[path = "broker_test_support/broker_host.rs"]
mod broker_host;
#[path = "broker_test_support/tenant_resolver.rs"]
mod tenant_resolver;
#[path = "broker_test_support/tls_provider.rs"]
mod tls_provider;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use broker_host::{build_broker_test_env, BrokerTestConfig};
use jid::BareJid;
use tempfile::TempDir;
use tenant_resolver::StaticTenantResolver;
use tls_provider::{read_http_request, LoopbackTlsServer};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use waddle_broker::admission::service::TraceRecorder;
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::{AdmissionQueryStore, AttemptStatus};
use waddle_broker::idp_proof::DeviceKeyRegistry;
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{AuthzObjectType, AuthzSubject, AuthzSubjectType};
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceError, HostInferenceErrorCode, HostInferenceMessage,
    HostInferenceRequest, InferenceCredentialRef, StreamItem,
};
use waddle_extensions::{PluginId, WaddleId};
use waddle_server::server::extension_inference_host::{
    BrokerExtensionInferenceHost, RoomDisclosureEmitter, RoomDisclosureEvent,
};

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
    PluginId,
) {
    let dir = TempDir::new().expect("tempdir");
    let account_id = AccountId::new_v4();
    let permission_grant = grant_tuple.then(|| {
        (
            AuthzObjectType::TenantCredential,
            AuthzSubject {
                subject_type: AuthzSubjectType::Account,
                id: account_id.to_string(),
                relation: None,
            },
        )
    });
    let (env, ledger, _fence) = build_broker_test_env(BrokerTestConfig {
        data_dir: dir.path(),
        trust_domain: "acme.example",
        broker_id: "broker-disclosure-test",
        spiffe_id: "spiffe://acme.example/ns/cells/sa/shared-pool",
        channel_binding: [0x42u8; 32],
        provider: "openai",
        deployment: "prod",
        model: "gpt-4o-mini",
        base_origin: "https://provider.acme.test",
        operation_path: "/v1/chat/completions",
        plugin_id: "ai-chatbot",
        credential_secret: b"sk-test-disclosure-secret",
        credential_account_id: None,
        permission_grant,
        device_keys: Arc::new(DeviceKeyRegistry::new()),
        trace: TraceRecorder::new(),
        server_addr,
        ca_pem,
    })
    .await;

    let timeline = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(RecordingDisclosureEmitter {
        timeline: Arc::clone(&timeline),
        events: Arc::clone(&events),
        ledger,
        fail_disclosure,
    });
    let host = env
        .build_host(
            Arc::new(StaticTenantResolver {
                account_id,
                credential_id: env.credential_id,
            }),
            250,
        )
        .with_disclosure_emitter(emitter);

    (dir, host, timeline, events, env.tenant_id, env.plugin_id)
}

#[tokio::test]
async fn room_disclosure_fires_post_admission_pre_egress_and_skips_denied_calls() {
    let tls = LoopbackTlsServer::bind("provider.acme.test").await;
    let egress_hits = Arc::new(AtomicUsize::new(0));
    let fail_disclosure = Arc::new(AtomicBool::new(false));

    let (_dir, host, timeline, events, tenant_id, plugin_id) = setup_disclosure_host(
        tls.addr,
        tls.ca_pem.clone(),
        true,
        Arc::clone(&fail_disclosure),
    )
    .await;

    let timeline_srv = Arc::clone(&timeline);
    let egress_hits_srv = Arc::clone(&egress_hits);
    let listener = tls.listener;
    let acceptor = tls.acceptor;
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
        plugin_id,
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
    let (_dir2, denied_host, denied_timeline, denied_events, denied_tenant, denied_plugin) =
        setup_disclosure_host(
            tls.addr,
            tls.ca_pem,
            false,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    let denied_context = InvocationContext {
        tenant_id: Some(denied_tenant),
        plugin_id: denied_plugin,
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
