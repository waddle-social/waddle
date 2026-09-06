use super::super::effects::room::DurableRoomEffect;
#[cfg(feature = "clustering")]
use super::super::effects::room::ExternalRoomEffect;
use super::super::effects::{DurableEffect, Effect, ExternalEffect, PlanSink, RoomExecutionPath};
use super::*;
use crate::server::routes::websocket::tests::{
    create_test_websocket_state, register_test_connection,
};
use waddle_xmpp::muc::room_actor::{ChangeAffiliation, Join};
use waddle_xmpp::muc::room_registry_actor::CreateRoom;

fn message(room: &BareJid, sender: &FullJid) -> Message {
    let mut message = Message::new(Some(Jid::from(room.clone())));
    message.from = Some(Jid::from(sender.clone()));
    message.type_ = XmppMessageType::Groupchat;
    message.id = Some(xmpp_parsers::message::Id("planned-room-message".to_owned()));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "planned body".to_owned(),
    );
    message
}

#[tokio::test]
async fn plan_local_groupchat_prepares_archive_and_inboxes_without_delivery() {
    let state = create_test_websocket_state().await;
    let room: BareJid = "planned@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let bob: FullJid = "bob@example.com/web".parse().expect("bob");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "planned".to_owned(),
            channel_id: "planned".to_owned(),
            config: Default::default(),
        })
        .await
        .expect("create room");
    for (nick, jid) in [("alice", &alice), ("bob", &bob)] {
        actor
            .ask(ChangeAffiliation {
                jid: jid.to_bare(),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("persist member affiliation");
        actor
            .ask(Join {
                nick: nick.to_owned(),
                real_jid: jid.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
    }
    let snapshot = actor
        .ask(GetRoomSnapshot {
            sender_jid: alice.clone(),
        })
        .reply_timeout(std::time::Duration::from_secs(5))
        .await
        .expect("membership snapshot");
    for owner in [alice.to_bare(), bob.to_bare()] {
        assert!(
            snapshot.durable_recipient_bare_jids.contains(&owner),
            "fixture must include durable recipient {owner}"
        );
    }
    let (alice_tx, mut alice_rx) = tokio::sync::mpsc::channel(8);
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    register_test_connection(state.as_ref(), &alice, alice_tx).await;
    register_test_connection(state.as_ref(), &bob, bob_tx).await;
    let sink = PlanSink::new();
    let capture = crate::ingress_shadow::IngressEffectCapture::new(None);
    let mut deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
        state.as_ref(),
        None,
    );
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    let outcome = dispatch_to_room(&deps, room.clone(), message(&room, &alice), 0).await;
    assert!(outcome.frames.is_empty());
    assert!(alice_rx.try_recv().is_err());
    assert!(bob_rx.try_recv().is_err());
    let captured = capture.snapshot();
    assert!(captured.markers.is_empty());
    assert!(captured.room_scope.is_none());
    assert!(!captured.intents.is_empty());
    let (plan, execution) = sink.take();
    assert!(matches!(execution, RoomExecutionPath::Local { room: planned, .. } if planned == room));
    assert!(plan.iter().any(|effect| matches!(&effect.effect, Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat { room: planned, .. })) if planned == &room)));
    for (recipient, policy) in [
        (&alice, super::super::effects::PlanSuppressionPolicy::Always),
        (
            &bob,
            super::super::effects::PlanSuppressionPolicy::SenderOnly,
        ),
    ] {
        let route = plan.iter().find(|effect| matches!(&effect.effect, Effect::External(ExternalEffect::Delivery(super::super::effects::delivery::ExternalDeliveryEffect::RouteToPeer { jid, .. })) if jid == recipient)).expect("occupant route");
        assert_eq!(route.suppression, policy);
    }
    for owner in [alice.to_bare(), bob.to_bare()] {
        assert!(plan.iter().any(|effect| matches!(&effect.effect, Effect::Durable(DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox { owner: planned, .. })) if planned == &owner)));
    }
    assert!(plan.iter().any(|effect| matches!(
        &effect.effect,
        Effect::External(ExternalEffect::Direct(
            super::super::effects::direct::ExternalDirectEffect::PushInboxUpdate { .. }
        ))
    )));
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn plan_remote_groupchat_resolves_owner_without_a_proxy_bridge() {
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };
    let room: BareJid = "remote@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let store = Arc::new(InProcessClaimStore::new());
    let remote = NodeIdentity::new("remote", "epoch");
    store
        .acquire(
            &Entity::new(EntityType::RoomActor, room.to_string()),
            &remote,
        )
        .await
        .expect("room claim");
    let state =
        crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
            crate::clustering::ClusteringHandles {
                claim_store: Some(store),
                node_identity: Some(SharedNodeIdentity::new(NodeIdentity::new("local", "epoch"))),
                ..Default::default()
            },
            Arc::new(InMemorySmSessionRegistry::new()),
        )
        .await;
    let sink = PlanSink::new();
    let capture = crate::ingress_shadow::IngressEffectCapture::new(None);
    let mut deps = Deps::registry_only(&state.deps.protocol.connection_registry);
    deps.effects = &sink;
    deps.web_socket_state = Some(state.as_ref());
    deps.room_registry = Some(&state.deps.protocol.room_registry);
    deps.ingress_effect_capture = Some(capture.clone());
    let sender_entity = Entity::new(EntityType::UserActor, alice.to_bare().to_string());
    deps.ordered_relay_origin = Some(OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
        sender_entity,
        inbound_sequence: 1,
        handoff: None,
    });
    let outcome = dispatch_to_room(&deps, room.clone(), message(&room, &alice), 0).await;
    assert!(outcome.frames.is_empty());
    let captured = capture.snapshot();
    assert!(captured.markers.is_empty());
    assert!(captured.room_scope.is_none());
    let (plan, execution) = sink.take();
    assert!(
        matches!(execution, RoomExecutionPath::Remote { room: planned, .. } if planned == room)
    );
    assert!(matches!(
        &plan[0].effect,
        Effect::External(ExternalEffect::Room(ExternalRoomEffect::RelayMucProxy {
            reflect_replies_to_sender: true,
            ..
        }))
    ));
    assert!(capture.snapshot().intents.iter().any(|intent| matches!(intent, IngressEffectIntent::DispatchToRoomRemote { room: planned, .. } if planned == &room)));
}
