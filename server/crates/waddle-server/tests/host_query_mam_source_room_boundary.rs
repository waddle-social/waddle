//! T2-M6-03: Host `ExtensionHostAdapter::query_mam` rejects `room != source_room`,
//! roomless `source_room: None` room queries, and 1:1 DM `MamTarget::Conversation`.

use std::sync::Arc;

use tempfile::TempDir;
use waddle_extensions::config::ExtensionConfig;
use waddle_extensions::host_tools::{
    ExtensionHostTools, InvocationContext, InvocationKind, MamQuery, MamTarget,
};
use waddle_extensions::{ExtensionManager, PluginId, WaddleId};
use waddle_server::auth::{NativeUserStore, RegisterRequest};
use waddle_server::permissions::{Object, ObjectType, Relation, Subject, Tuple, WriteTuple};
use waddle_server::server::build_websocket_state_for_test;
use waddle_server::server::extension_host_adapter::ExtensionHostAdapter;

#[tokio::test]
async fn host_query_mam_source_room_boundary_rejects_cross_room_roomless_and_conversation() {
    let cache = TempDir::new().expect("tempdir");
    let extension_manager = Arc::new(
        ExtensionManager::from_config(ExtensionConfig {
            enabled: false,
            cache_dir: cache.path().display().to_string(),
            modules: Vec::new(),
        })
        .await
        .expect("extension manager"),
    );

    let (state, _shutdown) =
        build_websocket_state_for_test(extension_manager, "example.com", 5).await;

    let alice_bare: jid::BareJid = "alice@example.com".parse().expect("alice");
    let native_user_store =
        NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone());
    native_user_store
        .register(RegisterRequest {
            username: "alice".to_owned(),
            domain: "example.com".to_owned(),
            password: "test-password".to_owned(),
            email: None,
        })
        .await
        .expect("seed alice");

    let room_a: jid::BareJid = "room-a@muc.example.com".parse().expect("room-a");
    let room_b: jid::BareJid = "room-b@muc.example.com".parse().expect("room-b");
    let peer: jid::BareJid = "bob@example.com".parse().expect("bob");

    let room_a_actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
            room_jid: room_a.clone(),
            waddle_id: "example.com".to_owned(),
            channel_id: "room-a".to_owned(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("create room-a");
    room_a_actor
        .ask(waddle_xmpp::muc::room_actor::ChangeAffiliation {
            jid: alice_bare.clone(),
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("grant member in room-a");

    for channel_id in ["room-a", "room-b"] {
        state
            .deps
            .app_state
            .permission_actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Channel, channel_id),
                    Relation::new("member"),
                    Subject::user(alice_bare.as_str()),
                ),
            })
            .await
            .expect("grant channel member tuple");
    }

    let host = ExtensionHostAdapter::new(Arc::clone(&state));
    let ctx_room_a = InvocationContext {
        tenant_id: None,
        waddle_id: WaddleId::try_new("example.com").expect("waddle"),
        plugin_id: PluginId::try_new("ai-chatbot").expect("plugin"),
        requester: Some(alice_bare.clone()),
        source_room: Some(room_a.clone()),
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    };

    // Matching source_room succeeds.
    host.query_mam(
        &ctx_room_a,
        MamQuery {
            target: MamTarget::Room(room_a.clone()),
            start: None,
            end: None,
            thread_id: None,
            sender: None,
            text: None,
            max_results: 10,
        },
    )
    .await
    .expect("matching source_room MAM query must succeed");

    // Cross-room (`room_b != source_room`) is rejected before storage read.
    let cross_room = host
        .query_mam(
            &ctx_room_a,
            MamQuery {
                target: MamTarget::Room(room_b),
                start: None,
                end: None,
                thread_id: None,
                sender: None,
                text: None,
                max_results: 10,
            },
        )
        .await;
    assert!(
        cross_room.is_err(),
        "host query_mam must reject room != source_room"
    );

    // 1:1 DM MamTarget::Conversation is unconditionally rejected.
    let dm_target = host
        .query_mam(
            &ctx_room_a,
            MamQuery {
                target: MamTarget::Conversation(peer),
                start: None,
                end: None,
                thread_id: None,
                sender: None,
                text: None,
                max_results: 10,
            },
        )
        .await;
    assert!(
        dm_target.is_err(),
        "host query_mam must reject 1:1 DM MamTarget::Conversation"
    );

    // Roomless (`source_room: None`) room query is rejected.
    let mut ctx_roomless = ctx_room_a;
    ctx_roomless.source_room = None;
    let roomless = host
        .query_mam(
            &ctx_roomless,
            MamQuery {
                target: MamTarget::Room(room_a),
                start: None,
                end: None,
                thread_id: None,
                sender: None,
                text: None,
                max_results: 10,
            },
        )
        .await;
    assert!(
        roomless.is_err(),
        "host query_mam must reject room queries when source_room is None"
    );
}
