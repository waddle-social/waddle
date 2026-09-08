use super::*;
use crate::server::routes::interpret::effects::{
    delivery::ExternalDeliveryEffect, Effect, ExternalEffect, PlanSink,
};
use waddle_xmpp::muc::room_actor::Join;
use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity};

#[tokio::test]
async fn xep_0045_room_plan_foreign_occupant_uses_room_origin_and_gate_keeps_caller() {
    let (registry, actor, room, claims, fence, store) = spawn_subject_mutation_test_room().await;
    // The subject fixture fixes every commit at revision 1; two occupancy
    // projections require distinct revisions on the same room incarnation.
    actor
        .ask(waddle_xmpp::muc::room_actor::RestoreDurableRoomState {
            store: Arc::new(OccupancyProjectionStore {
                inner: store,
                lifecycle: waddle_xmpp::muc::RoomLifecycleId::generate(),
                revision: std::sync::Mutex::new(waddle_xmpp::muc::RoomRevision::initial()),
            }),
            claim_fence: fence.clone(),
        })
        .await
        .expect("install monotonic projection store");
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("sender");
    let bob: jid::FullJid = "bob@example.com/web".parse().expect("remote occupant");
    for (nick, jid) in [("alice", &alice), ("bob", &bob)] {
        actor
            .ask(Join {
                nick: nick.to_owned(),
                real_jid: jid.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join occupant");
    }
    claims
        .acquire(
            &Entity::new(EntityType::UserActor, bob.to_bare().to_string()),
            &NodeIdentity::new("remote", "epoch"),
        )
        .await
        .expect("foreign user claim");
    let state =
        crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
            crate::clustering::ClusteringHandles {
                claim_store: Some(claims),
                node_identity: Some(SharedNodeIdentity::new(fence.owner)),
                ordered_relay_delivery_bridge: Some(
                    crate::clustering::route_bridge::OrderedRelayDeliveryBridge::new(
                        tokio_util::sync::CancellationToken::new(),
                        &crate::config::ClusteringMessagingConfig::default(),
                    ),
                ),
                ..Default::default()
            },
            Arc::new(InMemorySmSessionRegistry::new()),
        )
        .await;
    let sink = PlanSink::new();
    let sender_entity = Entity::new(EntityType::UserActor, alice.to_bare().to_string());
    let caller = OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::SmSession(
            waddle_xmpp::pending_delivery::SmSessionId::new("caller-stream"),
        ),
        sender_entity: sender_entity.clone(),
        inbound_sequence: 17,
        handoff: None,
    };
    let mut deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
        state.as_ref(),
        None,
    )
    .with_ordered_relay_origin(Some(caller));
    deps.effects = &sink;
    deps.room_registry = Some(&registry);
    let mut message = Message::new(Some(room.clone().into()));
    message.from = Some(alice.clone().into());
    message.type_ = XmppMessageType::Groupchat;
    message
        .bodies
        .insert(Default::default(), "room body".to_owned());
    dispatch_to_room(&deps, room.clone(), message.clone(), 0).await;
    let (effects, _) = sink.take();
    let (origin, stanza) = effects
        .iter()
        .find_map(|effect| match &effect.effect {
            Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid {
                target,
                origin: Some(origin),
                stanza,
                ..
            })) if target == &bob => Some((origin, stanza)),
            _ => None,
        })
        .expect("foreign-owned occupant must have a planned relay");
    let room_entity = Entity::new(EntityType::RoomActor, room.to_string());
    assert!(
        matches!(&origin.kind, OrderedRelayRouteOriginKind::Entity(entity) if entity == &room_entity)
    );
    assert_eq!(origin.sender_entity, room_entity);
    assert_eq!(origin.inbound_sequence, 0);
    assert!(origin.handoff.is_none());
    let Stanza::Message(copy) = stanza.as_ref() else {
        panic!("occupant copy must be a message");
    };
    assert_eq!(copy.type_, XmppMessageType::Groupchat);
    assert_eq!(copy.to, Some(bob.into()));
    assert_eq!(
        copy.from,
        Some(room.with_resource_str("alice").expect("room nick").into())
    );

    // The gate runs before the room-origin fanout scope and answers the
    // originating connection, retaining its inbound stream provenance.
    message.from = Some("outsider@example.com/web".parse().expect("non-occupant"));
    dispatch_to_room(&deps, room, message, 0).await;
    let (effects, _) = sink.take();
    assert!(effects.iter().any(|effect| matches!(
        &effect.effect,
        Effect::External(ExternalEffect::Frame(stanza))
            if matches!(stanza.as_ref(), Stanza::Message(reply) if reply.type_ == XmppMessageType::Error)
    )));
    assert!(!effects.iter().any(|effect| matches!(
        &effect.effect,
        Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RelayFullJid { .. }
        ))
    )));
    let origin = deps.ordered_relay_origin.as_ref().expect("caller origin");
    assert!(matches!(
        origin.kind,
        OrderedRelayRouteOriginKind::SmSession(_)
    ));
    assert_eq!(origin.sender_entity, sender_entity);
    assert_eq!(origin.inbound_sequence, 17);
}

struct OccupancyProjectionStore {
    inner: Arc<SubjectMutationStore>,
    lifecycle: waddle_xmpp::muc::RoomLifecycleId,
    revision: std::sync::Mutex<waddle_xmpp::muc::RoomRevision>,
}

impl waddle_xmpp::muc::MucDurableStore for OccupancyProjectionStore {
    fn load_room_state_fenced<'a>(
        &'a self,
        room: &'a jid::BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, Option<waddle_xmpp::muc::DurableRoomState>> {
        self.inner.load_room_state_fenced(room, fence)
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        room: &'a jid::BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, bool> {
        self.inner.check_exact_claim_fence(room, fence)
    }

    fn commit_room_mutation<'a>(
        &'a self,
        room: &'a jid::BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        intent: waddle_xmpp::muc::RoomDurableMutation,
        effects: waddle_xmpp::muc::RoomMutationEffects,
    ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
        Box::pin(async move {
            let mut outcome = self
                .inner
                .commit_room_mutation(room, fence, intent, effects)
                .await?;
            let mut revision = self.revision.lock().expect("projection revision lock");
            outcome.coordinates = waddle_xmpp::muc::RoomCommittedCoordinates {
                lifecycle: self.lifecycle,
                revision: *revision,
            };
            *revision = revision.next().expect("test revision overflow");
            Ok(outcome)
        })
    }
}
