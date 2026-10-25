use super::{
    destroy_space_channel, get_xmpp_channel, run_create, run_delete, write_channel_parent_tuple,
    AppState, Arc, BareJid, ChannelInfo, ChannelSpaceLink, Object, ObjectType, Relation, SpaceRef,
    SpacesCreateArgs, SpacesDeleteArgs, Subject, SubjectType,
};
use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
use crate::permissions::ListSubjects;
use crate::server::xmpp_state::{upsert_xmpp_channel, XmppChannelUpsert};
use waddle_xmpp::muc::PinPermission;

async fn fresh_state() -> AppState {
    let pool = DatabasePool::try_new(DatabaseConfig::default(), PoolConfig)
        .await
        .expect("pool");
    MigrationRunner::global()
        .run(pool.global())
        .await
        .expect("migrations");
    AppState::new(Arc::new(pool))
}

async fn space(state: &AppState, name: &str) -> SpaceRef {
    run_create(
        state,
        &SpacesCreateArgs {
            name: name.to_owned(),
            description: None,
            icon_url: None,
        },
    )
    .await
    .unwrap_or_else(|_| panic!("create space"))
}

async fn catalog_channel(state: &AppState) -> BareJid {
    upsert_xmpp_channel(
        state.db_pool.global_actor().clone(),
        &XmppChannelUpsert {
            id: "dormant".to_owned(),
            name: "Dormant forum".to_owned(),
            description: Some("Topic".to_owned()),
            channel_type: "forum".to_owned(),
            position: 7,
            is_default: false,
            pin_permission: PinPermission::Anyone,
            members_only: true,
            public_room: false,
        },
    )
    .await
    .expect("catalog row");
    waddle_xmpp::managed_room_jid("dormant", state.muc_domain.as_str()).expect("room JID")
}

async fn link(state: &AppState, channel_jid: &BareJid, space: &SpaceRef) {
    state
        .channel_space_link_store
        .set(&ChannelSpaceLink {
            channel_jid: channel_jid.clone(),
            space_jid: space.space_jid.clone(),
            space_node: space.space_node.clone(),
            created_at: 0,
        })
        .await
        .expect("link");
    write_channel_parent_tuple(state, "dormant", &space.space_node)
        .await
        .unwrap_or_else(|_| panic!("parent tuple"));
}

fn delete_args(space: &SpaceRef) -> SpacesDeleteArgs {
    SpacesDeleteArgs {
        space_jid: space.space_jid.clone(),
        space_node: Some(space.space_node.clone()),
        confirm: "yes".to_owned(),
    }
}

#[tokio::test]
async fn cascade_removes_dormant_catalog_channel_without_a_bookmark_or_actor() {
    let state = fresh_state().await;
    let space = space(&state, "Delete").await;
    let jid = catalog_channel(&state).await;
    link(&state, &jid, &space).await;
    run_delete(&state, &delete_args(&space))
        .await
        .unwrap_or_else(|_| panic!("cascade deletion"));
    assert!(
        get_xmpp_channel(state.db_pool.global_actor().clone(), "dormant")
            .await
            .expect("catalog")
            .is_none()
    );
    assert!(state
        .channel_space_link_store
        .get(&jid)
        .await
        .expect("link")
        .is_none());
}

#[tokio::test]
async fn failed_cascade_restores_catalog_link_bookmark_and_parent_tuple() {
    let state = fresh_state().await;
    let space = space(&state, "Keep").await;
    let jid = catalog_channel(&state).await;
    link(&state, &jid, &space).await;
    state.room_registry.kill();
    state.room_registry.wait_for_shutdown().await;
    assert!(run_delete(&state, &delete_args(&space)).await.is_err());
    let record = get_xmpp_channel(state.db_pool.global_actor().clone(), "dormant")
        .await
        .expect("catalog")
        .expect("restored row");
    assert_eq!(record.name, "Dormant forum");
    assert_eq!(record.description.as_deref(), Some("Topic"));
    assert_eq!(record.position, 7);
    assert_eq!(record.channel_type, "forum");
    assert_eq!(record.pin_permission, PinPermission::Anyone);
    assert!(record.members_only);
    assert!(!record.public_room);
    assert_eq!(
        state
            .channel_space_link_store
            .get(&jid)
            .await
            .expect("link")
            .expect("restored link")
            .space_node,
        space.space_node
    );
    let items = state
        .pubsub_storage
        .get_items(&state.spaces_jid, &space.space_node, None, &[])
        .await
        .expect("bookmarks");
    assert!(items.iter().any(|item| item.id == jid.to_string()));
    let parents = state
        .permission_actor
        .ask(ListSubjects {
            object: Object::new(ObjectType::Channel, "dormant"),
            relation: Relation::new("parent"),
        })
        .await
        .expect("parents");
    assert!(parents.contains(&Subject::userset(
        SubjectType::Space,
        space.space_node.as_str(),
        ""
    )));
}

#[tokio::test]
async fn external_bookmark_destruction_preserves_same_named_local_catalog_channel() {
    let state = fresh_state().await;
    catalog_channel(&state).await;
    destroy_space_channel(
        &state,
        &"dormant@remote.example".parse().expect("remote room"),
    )
    .await
    .unwrap_or_else(|_| panic!("remove external target"));
    assert!(
        get_xmpp_channel(state.db_pool.global_actor().clone(), "dormant")
            .await
            .expect("catalog")
            .is_some()
    );
}

#[tokio::test]
async fn old_space_bookmark_does_not_destroy_channel_linked_to_another_space() {
    let state = fresh_state().await;
    let old_space = space(&state, "Old").await;
    let current_space = space(&state, "Current").await;
    let jid = catalog_channel(&state).await;
    link(&state, &jid, &current_space).await;
    let item = waddle_xmpp::xep::build_channel_item(
        &ChannelInfo {
            id: "dormant".to_owned(),
            name: "Dormant forum".to_owned(),
            channel_type: "forum".to_owned(),
        },
        state.muc_domain.as_str(),
    )
    .expect("bookmark");
    state
        .pubsub_storage
        .publish_item(&state.spaces_jid, &old_space.space_node, &item, None, false)
        .await
        .expect("old bookmark");
    run_delete(&state, &delete_args(&old_space))
        .await
        .unwrap_or_else(|_| panic!("old space deletion"));
    assert!(
        get_xmpp_channel(state.db_pool.global_actor().clone(), "dormant")
            .await
            .expect("catalog")
            .is_some()
    );
    assert_eq!(
        state
            .channel_space_link_store
            .get(&jid)
            .await
            .expect("link")
            .expect("current link")
            .space_node,
        current_space.space_node
    );
}
