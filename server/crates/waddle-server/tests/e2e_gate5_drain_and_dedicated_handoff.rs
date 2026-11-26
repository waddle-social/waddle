//! T4-S12: Release Evidence Gate 1/5 (M6) — `WADDLE_DRAIN_TIMEOUT_SECS` pod
//! drain plus shared-to-dedicated cell handoff via `TursoPlacementLeaseStore`
//! and `BrokerReplicaFence`, aborting old shared-pool inference streams while
//! admitting fresh inference on the dedicated cell holder cleanly.

#[path = "broker_test_support/broker_host.rs"]
mod broker_host;
#[path = "broker_test_support/tenant_resolver.rs"]
mod tenant_resolver;
#[path = "broker_test_support/tls_provider.rs"]
mod tls_provider;

use std::sync::Arc;
use std::time::Duration;

use broker_host::{build_broker_test_env, BrokerTestConfig};
use tempfile::TempDir;
use tenant_resolver::StaticTenantResolver;
use tls_provider::{read_http_request, LoopbackTlsServer};
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use waddle_broker::admission::service::TraceRecorder;
use waddle_broker::idp_proof::DeviceKeyRegistry;
use waddle_broker_core::admission::{FenceKey, FenceKind};
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{AccountId, CellRole, SpiffeId};
use waddle_broker_core::permission::{AuthzObjectType, AuthzSubject, AuthzSubjectType};
use waddle_broker_core::placement::{PlacementLeaseStore, PlacementMode};
use waddle_ecdysis::GracefulShutdown;
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceErrorCode, HostInferenceMessage, HostInferenceRequest,
    InferenceCredentialRef, StreamItem,
};
use waddle_extensions::WaddleId;
use waddle_server::server::extension_inference_host::{
    BrokerCredentialResolver, BrokerExtensionInferenceHost,
};
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;
use waddle_tenancy::quiesce::drain_timeout_from_env;

#[tokio::test]
async fn gate5_graceful_drain_and_shared_to_dedicated_cell_handoff() {
    let budget = drain_timeout_from_env();
    let shutdown = GracefulShutdown::new(budget);
    let stop_token = shutdown.stop_token();
    shutdown.trigger_stop();
    tokio::time::timeout(Duration::from_secs(2), stop_token.cancelled())
        .await
        .expect("graceful drain token cancelled promptly");

    let tls = LoopbackTlsServer::bind("provider.acme.test").await;
    let listener = tls.listener;
    let acceptor = tls.acceptor;
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
    let account_id = AccountId::new_v4();
    let (env, _ledger, replica_fence) = build_broker_test_env(BrokerTestConfig {
        data_dir: dir.path(),
        trust_domain: "acme.example",
        broker_id: "broker-gate5-handoff",
        spiffe_id: "spiffe://acme.example/ns/cells/sa/shared-pool",
        channel_binding: [0x11u8; 32],
        provider: "openai",
        deployment: "prod",
        model: "gpt-4o-mini",
        base_origin: "https://provider.acme.test",
        operation_path: "/v1/chat/completions",
        plugin_id: "ai-chatbot",
        credential_secret: b"sk-test-handoff-secret",
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
        server_addr: tls.addr,
        ca_pem: tls.ca_pem,
    })
    .await;

    let lease_store = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("leases.db"))
            .await
            .expect("lease store"),
    );
    let spiffe_dedicated =
        SpiffeId::parse("spiffe://acme.example/ns/cells/sa/dedicated-acme").expect("spiffe ded");
    let binding_dedicated = [0x22u8; 32];
    let kp_dedicated = HolderKeypair::generate().expect("kp dedicated");
    let holder_dedicated = kp_dedicated.sign_channel_binding(&spiffe_dedicated, &binding_dedicated);

    let shared_role = CellRole::parse("shared-pool").expect("shared cell role");
    let dedicated_role = CellRole::parse("dedicated-acme").expect("dedicated cell role");
    let shared_lease = lease_store
        .claim(
            &env.tenant_id,
            &shared_role,
            PlacementMode::SharedPool,
            &env.holder,
            30_000,
        )
        .await
        .expect("claim shared lease");

    let resolver: Arc<dyn BrokerCredentialResolver> = Arc::new(StaticTenantResolver {
        account_id,
        credential_id: env.credential_id,
    });
    let shared_host = env.build_host(Arc::clone(&resolver), 100);
    let mut dedicated_cfg = env.build_host_config(100);
    dedicated_cfg.holder = holder_dedicated.clone();
    dedicated_cfg.peer_spiffe_id = spiffe_dedicated;
    dedicated_cfg.channel_binding = binding_dedicated;
    let dedicated_host = BrokerExtensionInferenceHost::new(
        dedicated_cfg,
        Arc::clone(&env.admission),
        Arc::clone(&env.egress),
        resolver,
    );

    let ctx = InvocationContext {
        tenant_id: Some(env.tenant_id.clone()),
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
    let tenant_c = env.tenant_id.clone();
    let hs = env.holder.clone();
    let hd = holder_dedicated.clone();
    let placement_key = FenceKey {
        tenant_id: env.tenant_id.clone(),
        kind: FenceKind::Placement,
    };
    env.admission
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
