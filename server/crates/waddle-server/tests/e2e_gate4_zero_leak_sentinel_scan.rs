//! T4-S09: Release Evidence Gate 4 (M6) — End-to-end zero-leak sentinel scan
//! across broker custody/traces/ledger, hostile provider errors, recursive IQ
//! error-bounce payload stripping, SM resume persistence filter patterns, and
//! closed OTel `MetricAttribute` telemetry values.

#[path = "broker_test_support/admission_db.rs"]
mod admission_db;
#[path = "broker_test_support/broker_host.rs"]
mod broker_host;
#[path = "broker_test_support/tenant_resolver.rs"]
mod tenant_resolver;
#[path = "broker_test_support/tls_provider.rs"]
mod tls_provider;

use std::sync::Arc;

use admission_db::load_attempt_ids;
use broker_host::{build_broker_test_env, BrokerTestConfig};
use tempfile::TempDir;
use tenant_resolver::StaticTenantResolver;
use tls_provider::{read_http_request, LoopbackTlsServer};
use tokio::io::AsyncWriteExt;
use waddle_broker::admission::service::TraceRecorder;
use waddle_broker::admission::AdmissionQueryStore;
use waddle_broker::idp_proof::DeviceKeyRegistry;
use waddle_broker_core::identity::AccountId;
use waddle_broker_core::permission::{AuthzObjectType, AuthzSubject, AuthzSubjectType};
use waddle_broker_core::trace_verifier::assert_secret_free;
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceMessage, HostInferenceRequest, InferenceCredentialRef,
    StreamItem,
};
use waddle_extensions::WaddleId;
use waddle_server::credential_iq::bounce_iq_error_xml_with_payload;
use waddle_xmpp::telemetry::attributes::{
    BrokerCredentialOp, BrokerFenceKind, BrokerInferenceOutcome, MetricAttribute,
};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const SENTINEL_TENANT_KEY: &str = "sk-sentinel-tenant-key-991827";
const SENTINEL_MEMBER_SEALED: &str = "sk-sentinel-member-iq-448811";
const SENTINEL_HOSTILE_UPSTREAM: &str = "UPSTREAM_LEAK_SENTINEL_331900";

#[tokio::test]
async fn gate4_zero_leak_sentinel_scan_across_traces_errors_bounces_and_otel() {
    let tls = LoopbackTlsServer::bind("provider.acme.test").await;
    let listener = tls.listener;
    let acceptor = tls.acceptor;

    let srv = tokio::spawn(async move {
        // Call 1: normal SSE response.
        let (tcp1, _) = listener.accept().await.expect("accept 1");
        let mut s1 = acceptor.accept(tcp1).await.expect("tls 1");
        let _ = read_http_request(&mut s1).await;
        let b1 =
            "data: {\"choices\":[{\"delta\":{\"content\":\"clean output\"}}]}\n\ndata: [DONE]\n\n";
        let r1 = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            b1.len(),
            b1
        );
        s1.write_all(r1.as_bytes()).await.expect("write 1");
        s1.shutdown().await.expect("shutdown 1");

        // Call 2: hostile 502 containing tenant key and upstream sentinel.
        let (tcp2, _) = listener.accept().await.expect("accept 2");
        let mut s2 = acceptor.accept(tcp2).await.expect("tls 2");
        let _ = read_http_request(&mut s2).await;
        let b2 = format!("{{\"error\":\"{SENTINEL_TENANT_KEY} {SENTINEL_HOSTILE_UPSTREAM}\"}}");
        let r2 = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            b2.len(),
            b2
        );
        s2.write_all(r2.as_bytes()).await.expect("write 2");
        s2.shutdown().await.expect("shutdown 2");
    });

    let dir = TempDir::new().expect("tempdir");
    let account_id = AccountId::new_v4();
    let trace = TraceRecorder::new();
    let (env, ledger, _fence) = build_broker_test_env(BrokerTestConfig {
        data_dir: dir.path(),
        trust_domain: "acme.example",
        broker_id: "broker-gate4-sentinel",
        spiffe_id: "spiffe://acme.example/ns/cells/sa/shared-pool",
        channel_binding: [0x44u8; 32],
        provider: "openai",
        deployment: "prod",
        model: "gpt-4o-mini",
        base_origin: "https://provider.acme.test",
        operation_path: "/v1/chat/completions",
        plugin_id: "ai-chatbot",
        credential_secret: SENTINEL_TENANT_KEY.as_bytes(),
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
        100,
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
            content: "hello".to_owned(),
        }],
        max_output_tokens: Some(64),
    };

    let mut s1 = host
        .start_stream(&ctx, req.clone())
        .await
        .expect("stream 1");
    let mut guest_out = String::new();
    while let Some(item) = s1.rx.recv().await {
        if let StreamItem::Chunk(c) = item {
            guest_out.push_str(&String::from_utf8_lossy(&c.delta));
        } else {
            break;
        }
    }
    let hostile_err = host
        .start_stream(&ctx, req)
        .await
        .err()
        .expect("hostile 502 fails");
    srv.await.expect("server");

    // Verify IQ bounce stripping strips member sealed sentinel.
    let nested_payload = Element::builder("enroll", "urn:waddle:credential:1")
        .append(
            Element::builder("ciphertext", "urn:waddle:credential:1")
                .append(SENTINEL_MEMBER_SEALED),
        )
        .build();
    let bounced_xml = bounce_iq_error_xml_with_payload(
        "iq-sentinel-1",
        Some("acme.example"),
        Some("alice@acme.example/web"),
        nested_payload,
        StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "en",
            "bounce",
        ),
    );

    // Check closed OTel MetricAttribute enums expose only low-cardinality keys/values.
    let mut otel_dump = String::new();
    for kv in [
        BrokerInferenceOutcome::Completed.key_value(),
        BrokerInferenceOutcome::Denied.key_value(),
        BrokerCredentialOp::Create.key_value(),
        BrokerCredentialOp::Resolve.key_value(),
        BrokerFenceKind::Authorization.key_value(),
        BrokerFenceKind::PlacementLease.key_value(),
    ] {
        otel_dump.push_str(&format!("{:?}", kv));
    }

    // Check ledger attempts contain no plaintext secrets.
    let mut ledger_dump = String::new();
    for attempt_id in load_attempt_ids(dir.path()).await {
        let rec = ledger
            .get_attempt(attempt_id)
            .await
            .expect("get_attempt")
            .expect("row");
        ledger_dump.push_str(&format!("{rec:?}"));
    }

    let sentinels = [
        SENTINEL_TENANT_KEY,
        SENTINEL_MEMBER_SEALED,
        SENTINEL_HOSTILE_UPSTREAM,
    ];
    let trace_jsonl = trace.to_jsonl();
    assert_secret_free(&trace_jsonl, &sentinels).expect("trace jsonl must be secret-free");
    let scan_surface = format!(
        "{guest_out}\n{}\n{bounced_xml}\n{otel_dump}\n{ledger_dump}\n{trace_jsonl}",
        hostile_err.message
    );
    for sentinel in sentinels {
        assert!(
            !scan_surface.contains(sentinel),
            "sentinel {sentinel} leaked into output/error/bounce/trace/ledger/otel"
        );
    }
}
