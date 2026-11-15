use std::sync::Arc;

use jid::{BareJid, FullJid};
use waddle_xmpp::muc::room_actor::{ChangeAffiliation, JoinAffiliationGrant, JoinWithAffiliation};
use waddle_xmpp::muc::RoomConfig;
use waddle_xmpp::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::Affiliation;
use waddle_xmpp_core::OccupancySessionGeneration;

use super::{
    local_event_planning_access, relayed_event_planning_access, remote_event_planning_access,
    TestEventPlanningOwnerRelay, TEST_EVENT_PLANNING_OWNER_RELAY,
};
use crate::clustering::ClusteringHandles;
use crate::server::routes::websocket::cleanup::get_or_create_room_actor;
use crate::server::routes::websocket::test_state::{
    create_test_websocket_state_with_extension_manager, empty_extension_manager, TestStateOverrides,
};

#[tokio::test]
async fn remote_workspace_access_uses_owner_membership_and_observes_revocation() {
    let room: BareJid = "london@muc.example.com".parse().expect("room");
    let owner = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides::default(),
    )
    .await;
    let actor = get_or_create_room_actor(
        &owner,
        &room,
        RoomConfig::default(),
        "default".into(),
        "london".into(),
    )
    .await
    .expect("room actor")
    .actor_ref;
    let alice: FullJid = "alice@example.com/android".parse().expect("alice");
    let outsider: FullJid = "outsider@example.com/android".parse().expect("outsider");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".into(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".into(),
            admission_revision: 0,
            session: OccupancySessionGeneration::mint(),
        })
        .await
        .expect("member joins");
    let claims: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    claims
        .acquire(
            &Entity::new(EntityType::RoomActor, room.to_string()),
            &NodeIdentity::new("owner-node", "owner-epoch"),
        )
        .await
        .expect("remote owner claim");
    let origin = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            clustering: Some(ClusteringHandles {
                claim_store: Some(claims),
                node_identity: Some(SharedNodeIdentity::new(NodeIdentity::new(
                    "origin-node",
                    "origin-epoch",
                ))),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await;
    let relay: TestEventPlanningOwnerRelay = Arc::new(move |message| {
        let owner = Arc::clone(&owner);
        Box::pin(async move { Ok(relayed_event_planning_access(&owner, message).await) })
    });
    let replacement_state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides::default(),
    )
    .await;
    let replacement_actor = get_or_create_room_actor(
        &replacement_state,
        &room,
        RoomConfig::default(),
        "default".into(),
        "london".into(),
    )
    .await
    .expect("replacement room actor")
    .actor_ref;
    let replacement_node = local_event_planning_access(&replacement_actor, &alice)
        .await
        .expect("replacement access")
        .storage_node();
    let expected_node = local_event_planning_access(&actor, &alice)
        .await
        .expect("local access")
        .storage_node();
    assert_ne!(
        replacement_node, expected_node,
        "a fresh room incarnation at the same JID must use different backing storage"
    );
    TEST_EVENT_PLANNING_OWNER_RELAY
        .scope(relay, async {
            let remote = remote_event_planning_access(&origin, &room, &alice)
                .await
                .expect("remote access")
                .expect("remote room");
            assert_eq!(remote.storage_node(), expected_node);
            assert_eq!(
                remote_event_planning_access(&origin, &room, &alice)
                    .await
                    .map(|access| access.map(|access| access.affiliation)),
                Ok(Some(Affiliation::Member))
            );
            assert_eq!(
                remote_event_planning_access(&origin, &room, &outsider)
                    .await
                    .map(|access| access.map(|access| access.affiliation)),
                Ok(Some(Affiliation::None))
            );
            actor
                .ask(ChangeAffiliation {
                    jid: alice.to_bare(),
                    affiliation: Affiliation::None,
                })
                .await
                .expect("revoke member");
            let revoked = remote_event_planning_access(&origin, &room, &alice)
                .await
                .expect("revoked remote access")
                .expect("remote room");
            assert_eq!(
                revoked.storage_node(),
                expected_node,
                "membership changes must retain the room lifecycle"
            );
            assert_eq!(
                remote_event_planning_access(&origin, &room, &alice)
                    .await
                    .map(|access| access.map(|access| access.affiliation)),
                Ok(Some(Affiliation::None))
            );
        })
        .await;
}
