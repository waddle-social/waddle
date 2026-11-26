//! T3-06: Pairwise (F14 + F15 + F20) — Mid-stream room kick closes the room
//! fence (`BrokerReplicaFence`), aborts the active brokered inference stream,
//! and removes member channel access so `ExtensionHostAdapter::query_mam` returns
//! `Denied`.

#[path = "broker_test_support/broker_host.rs"]
mod broker_host;
#[path = "broker_test_support/tenant_resolver.rs"]
mod tenant_resolver;
#[path = "broker_test_support/tls_provider.rs"]
mod tls_provider;

use std::sync::Arc;
use std::time::Duration;

use broker_host::{build_broker_test_env, BrokerTestConfig};
use jid::BareJid;
use tempfile::TempDir;
use tenant_resolver::StaticTenantResolver;
use tls_provider::{read_http_request, LoopbackTlsServer};
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use waddle_broker::admission::service::TraceRecorder;
use waddle_broker::idp_proof::DeviceKeyRegistry;
use waddle_broker_core::admission::{FenceKey, FenceKind, RoomScopeKey};
use waddle_broker_core::identity::AccountId;
use waddle_broker_core::permission::{AuthzObjectType, AuthzSubject, AuthzSubjectType};
use waddle_extensions::config::ExtensionConfig;
use waddle_extensions::host_tools::{
    ExtensionHostTools, InvocationContext, InvocationKind, MamQuery, MamTarget,
};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceErrorCode, HostInferenceMessage, HostInferenceRequest,
    InferenceCredentialRef, StreamItem,
};
use waddle_extensions::{ExtensionManager, WaddleId};
use waddle_server::auth::{NativeUserStore, RegisterRequest};
use waddle_server::permissions::{
    DeleteTuple, Object, ObjectType, Relation, Subject, Tuple, WriteTuple,
};
use waddle_server::server::build_websocket_state_for_test;
use waddle_server::server::extension_host_adapter::ExtensionHostAdapter;

#[tokio::test]
async fn pairwise_room_kick_aborts_active_stream_and_revokes_mam_access() {
    let tls = LoopbackTlsServer::bind("provider.acme.test").await;
    let listener = tls.listener;
    let acceptor = tls.acceptor;
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
    let account_id = AccountId::new_v4();
    let (env, _ledger, replica_fence) = build_broker_test_env(BrokerTestConfig {
        data_dir: dir.path(),
        trust_domain: "example.com",
        broker_id: "broker-room-kick",
        spiffe_id: "spiffe://example.com/ns/cells/sa/shared-pool",
        channel_binding: [0x21u8; 32],
        provider: "openai",
        deployment: "prod",
        model: "gpt-4o-mini",
        base_origin: "https://provider.acme.test",
        operation_path: "/v1/chat/completions",
        plugin_id: "ai-chatbot",
        credential_secret: b"sk-test-room-kick",
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
    let inf_host = env.build_host(
        Arc::new(StaticTenantResolver {
            account_id,
            credential_id: env.credential_id,
        }),
        250,
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
        tenant_id: Some(env.tenant_id.clone()),
        waddle_id: WaddleId::try_new("example.com").expect("waddle"),
        plugin_id: env.plugin_id,
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
    env.admission
        .restrict_fence(
            &FenceKey {
                tenant_id: env.tenant_id,
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
