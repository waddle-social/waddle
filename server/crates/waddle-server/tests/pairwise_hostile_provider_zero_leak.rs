//! T3-09: Pairwise (F17 + F18 + F20) — Hostile upstream provider responses
//! (302 SSRF redirect + 502 error body containing provider secret sentinel)
//! fail closed with zero secret or metadata leak across error values and traces.

#[path = "broker_test_support/broker_host.rs"]
mod broker_host;
#[path = "broker_test_support/tenant_resolver.rs"]
mod tenant_resolver;
#[path = "broker_test_support/tls_provider.rs"]
mod tls_provider;

use std::sync::Arc;

use broker_host::{build_broker_test_env, BrokerTestConfig};
use tempfile::TempDir;
use tenant_resolver::StaticTenantResolver;
use tls_provider::{read_http_request, LoopbackTlsServer};
use tokio::io::AsyncWriteExt;
use waddle_broker::admission::service::TraceRecorder;
use waddle_broker::idp_proof::DeviceKeyRegistry;
use waddle_broker_core::identity::AccountId;
use waddle_broker_core::permission::{AuthzObjectType, AuthzSubject, AuthzSubjectType};
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceMessage, HostInferenceRequest, InferenceCredentialRef,
};
use waddle_extensions::WaddleId;

const CRED_SENTINEL: &str = "sk-waddle-hostile-cred-sentinel-8812";
const BODY_SENTINEL: &str = "LEAK_UPSTREAM_BODY_SENTINEL_7741";
const REDIRECT_SENTINEL: &str = "http://169.254.169.254/latest/meta-data/";

#[tokio::test]
async fn hostile_provider_302_metadata_and_502_sentinel_never_leak_secrets() {
    let tls = LoopbackTlsServer::bind("provider.acme.test").await;
    let listener = tls.listener;
    let acceptor = tls.acceptor;

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
    let account_id = AccountId::new_v4();
    let trace = TraceRecorder::new();
    let (env, _ledger, _fence) = build_broker_test_env(BrokerTestConfig {
        data_dir: dir.path(),
        trust_domain: "acme.example",
        broker_id: "broker-hostile-test",
        spiffe_id: "spiffe://acme.example/ns/cells/sa/shared-pool",
        channel_binding: [0x33u8; 32],
        provider: "openai",
        deployment: "prod",
        model: "gpt-4o-mini",
        base_origin: "https://provider.acme.test",
        operation_path: "/v1/chat/completions",
        plugin_id: "ai-chatbot",
        credential_secret: CRED_SENTINEL.as_bytes(),
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
        trace: trace.clone(),
        server_addr: tls.addr,
        ca_pem: tls.ca_pem,
    })
    .await;

    let host = env.build_host(
        Arc::new(StaticTenantResolver {
            account_id,
            credential_id: env.credential_id,
        }),
        250,
    );

    let ctx = InvocationContext {
        tenant_id: Some(env.tenant_id),
        waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
        plugin_id: env.plugin_id,
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
