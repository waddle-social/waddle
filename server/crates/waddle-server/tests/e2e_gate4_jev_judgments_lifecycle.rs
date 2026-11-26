//! T4-S08: Release Evidence Gate 3/4 (M6) — Unattended tenant-funded
//! `jev-judgments` lifecycle: ZDR/no-training route qualification, opt-in
//! room observation via `InferenceCredentialRef::Tenant`, loopback TLS streaming
//! judgment batch, and host-verified `Completed` usage reconciliation.

#[path = "broker_test_support/admission_db.rs"]
mod admission_db;
#[path = "broker_test_support/broker_host.rs"]
mod broker_host;
#[path = "broker_test_support/tls_provider.rs"]
mod tls_provider;

use std::sync::Arc;
use std::time::Duration;

use admission_db::load_attempt_ids;
use async_trait::async_trait;
use broker_host::{build_broker_test_env, BrokerTestConfig};
use jid::BareJid;
use tempfile::TempDir;
use tls_provider::{read_http_request, LoopbackTlsServer};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
use waddle_broker::admission::service::TraceRecorder;
use waddle_broker::admission::{AdmissionQueryStore, AttemptStatus};
use waddle_broker::egress::{
    DataCollectionPolicy, ProviderOperation as ZdrProviderOp, QualifiedZdrRoute, RouteKeyOrigin,
    RouteQualificationError, ZdrRouteQuery, ZdrRouteRegistry,
};
use waddle_broker::idp_proof::{AuthProvenance, DeviceKeyRegistry};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{AuthzObjectType, AuthzSubject, AuthzSubjectType};
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceError, HostInferenceErrorCode, HostInferenceMessage,
    HostInferenceRequest, InferenceCredentialRef, StreamItem,
};
use waddle_extensions::WaddleId;
use waddle_server::server::extension_inference_host::BrokerCredentialResolver;

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
    let tls = LoopbackTlsServer::bind("openrouter.acme.test").await;
    let listener = tls.listener;
    let acceptor = tls.acceptor;

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
    let (env, ledger, _fence) = build_broker_test_env(BrokerTestConfig {
        data_dir: dir.path(),
        trust_domain: "acme.example",
        broker_id: "broker-gate4-jev",
        spiffe_id: "spiffe://acme.example/ns/cells/sa/shared-pool",
        channel_binding: [0x7Au8; 32],
        provider: "openrouter",
        deployment: "default",
        model: "typesafe/jev-1.13",
        base_origin: "https://openrouter.acme.test",
        operation_path: "/api/v1/chat/completions",
        plugin_id: "jev-judgments",
        credential_secret: b"sk-tenant-jev-zdr-key",
        credential_account_id: None,
        permission_grant: Some((
            AuthzObjectType::TenantCredential,
            AuthzSubject {
                subject_type: AuthzSubjectType::Extension,
                id: "observation".to_owned(),
                relation: None,
            },
        )),
        device_keys: Arc::new(DeviceKeyRegistry::new()),
        trace: TraceRecorder::new(),
        server_addr: tls.addr,
        ca_pem: tls.ca_pem,
    })
    .await;
    let host = env.build_host(
        Arc::new(JevTenantResolver {
            credential_id: env.credential_id,
        }),
        180,
    );

    let mut opened = host
        .start_stream(
            &InvocationContext {
                tenant_id: Some(env.tenant_id),
                waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
                plugin_id: env.plugin_id,
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

    let attempt_id = load_attempt_ids(dir.path()).await[0];
    let rec = ledger
        .get_attempt(attempt_id)
        .await
        .expect("get attempt")
        .expect("attempt row");
    assert_eq!(rec.status, AttemptStatus::Completed);
}
