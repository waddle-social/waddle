//! T3-10: Pairwise (F15 + F16 + F20) — Observer (`jev-judgments`) cannot spend
//! member credentials, must use tenant credentials, and mid-stream egress drop
//! after send quarantines the attempt as `UnknownAfterSend` while conserving budget.

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

struct ObserverResolver {
    credential_id: Uuid,
}

#[async_trait]
impl BrokerCredentialResolver for ObserverResolver {
    async fn resolve_requester_account(
        &self,
        _tenant_id: &TenantId,
        _requester: &BareJid,
    ) -> Result<(AccountId, AuthProvenance), HostInferenceError> {
        Err(HostInferenceError::new(
            HostInferenceErrorCode::Denied,
            "observer has no interactive requester account",
        ))
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
        panic!("observer invoking_user credential must be rejected before resolver lookup");
    }
}

#[tokio::test]
async fn observer_denies_member_credential_and_quarantines_after_midstream_drop() {
    let tls = LoopbackTlsServer::bind("provider.acme.test").await;
    let listener = tls.listener;
    let acceptor = tls.acceptor;

    // Provider sends truncated chunked body then drops TCP abruptly without 0\r\n\r\n terminator.
    let srv = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut tls_stream = acceptor.accept(tcp).await.expect("tls accept");
        let _req = read_http_request(&mut tls_stream).await;
        let partial = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 4096\r\n\r\n{}",
            partial
        );
        tls_stream.write_all(resp.as_bytes()).await.expect("write");
        tls_stream.flush().await.expect("flush");
        drop(tls_stream);
    });

    let dir = TempDir::new().expect("tempdir");
    let (env, ledger, _fence) = build_broker_test_env(BrokerTestConfig {
        data_dir: dir.path(),
        trust_domain: "acme.example",
        broker_id: "broker-observer-test",
        spiffe_id: "spiffe://acme.example/ns/cells/sa/shared-pool",
        channel_binding: [0x66u8; 32],
        provider: "openrouter",
        deployment: "default",
        model: "typesafe/jev-1.13",
        base_origin: "https://provider.acme.test",
        operation_path: "/v1/chat/completions",
        plugin_id: "jev-judgments",
        credential_secret: b"sk-test-observer-tenant",
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
        Arc::new(ObserverResolver {
            credential_id: env.credential_id,
        }),
        300,
    );

    let obs_ctx = InvocationContext {
        tenant_id: Some(env.tenant_id),
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        plugin_id: env.plugin_id,
        requester: Some("alice@acme.example".parse().expect("alice")),
        source_room: Some("room-obs@muc.acme.example".parse().expect("room")),
        kind: InvocationKind::RoomMessageObserve,
        provider_room_grants: Vec::new(),
    };

    // 1. Observer requesting InvokingUser credential MUST be rejected.
    let member_attempt = host
        .start_stream(
            &obs_ctx,
            HostInferenceRequest {
                credential: InferenceCredentialRef::InvokingUser("openrouter".to_owned()),
                provider: "openrouter".to_owned(),
                deployment: "default".to_owned(),
                model: "typesafe/jev-1.13".to_owned(),
                messages: vec![HostInferenceMessage {
                    role: "user".to_owned(),
                    content: "score messages".to_owned(),
                }],
                max_output_tokens: Some(128),
            },
        )
        .await
        .err()
        .expect("observer must be denied InvokingUser credential");
    assert_eq!(member_attempt.code, HostInferenceErrorCode::Denied);

    // 2. Observer with Tenant credential streams partial chunk, hits abrupt drop -> UnknownAfterSend quarantine.
    let mut stream = host
        .start_stream(
            &obs_ctx,
            HostInferenceRequest {
                credential: InferenceCredentialRef::Tenant("default".to_owned()),
                provider: "openrouter".to_owned(),
                deployment: "default".to_owned(),
                model: "typesafe/jev-1.13".to_owned(),
                messages: vec![HostInferenceMessage {
                    role: "user".to_owned(),
                    content: "score messages".to_owned(),
                }],
                max_output_tokens: Some(128),
            },
        )
        .await
        .expect("tenant-funded observer stream must admit");

    let mut saw_error = false;
    while let Some(item) = tokio::time::timeout(Duration::from_secs(5), stream.rx.recv())
        .await
        .expect("stream recv timeout")
    {
        if matches!(item, StreamItem::Error(_)) {
            saw_error = true;
            break;
        }
    }
    assert!(saw_error, "truncated upstream drop must emit stream error");
    srv.await.expect("server");

    let attempt_id = load_attempt_ids(dir.path()).await[0];
    let attempt = ledger
        .get_attempt(attempt_id)
        .await
        .expect("get attempt")
        .expect("attempt row");
    assert_eq!(attempt.status, AttemptStatus::Quarantined);
    assert_eq!(attempt.reserved_spend_microcents, 300);
}
