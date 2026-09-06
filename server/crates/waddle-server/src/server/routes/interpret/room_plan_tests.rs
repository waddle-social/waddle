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
    let capture = crate::ingress::IngressEffectCapture::new();
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
    assert!(!captured.overflowed);
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
    let capture = crate::ingress::IngressEffectCapture::new();
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
    assert!(!captured.overflowed);
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
    assert_eq!(
        plan[0].suppression,
        super::super::effects::PlanSuppressionPolicy::Always,
        "origin duplicates must reach owner reconciliation and sender reflection"
    );
    assert!(capture.snapshot().intents.iter().any(|intent| matches!(intent, IngressEffectIntent::DispatchToRoomRemote { room: planned, .. } if planned == &room)));
}

#[tokio::test]
async fn plan_pinned_retraction_freezes_system_archive_and_deliveries() {
    use super::super::effects::{
        delivery::ExternalDeliveryEffect,
        direct::{DurableDirectEffect, ExternalDirectEffect},
        room::{ExternalRoomEffect, RoomActorMutation},
        PlanSuppressionPolicy,
    };
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp::muc::pin::{PinChangeRequest, PinPreview, PinStateChange, PinnedEntry};
    use waddle_xmpp::muc::room_actor::{ApplyPin, GetPinList};
    use waddle_xmpp_core::xep0359::StanzaId;

    let state = create_test_websocket_state().await;
    let room: BareJid = "retraction@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let bob: FullJid = "bob@example.com/web".parse().expect("bob");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "retraction".to_owned(),
            channel_id: "retraction".to_owned(),
            config: Default::default(),
        })
        .await
        .expect("room");
    for (nick, jid) in [("alice", &alice), ("bob", &bob)] {
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
    let target = StanzaId::new("pinned-target", Jid::from(room.clone()));
    let pinned = PinnedEntry {
        target_stanza_id: target.clone(),
        pinner_jid: alice.to_bare(),
        pinned_at: chrono::Utc::now(),
        preview: PinPreview::new(
            alice.to_bare(),
            Some("alice".to_owned()),
            "private preview",
            chrono::Utc::now(),
        ),
    };
    actor
        .ask(ApplyPin {
            change: PinStateChange::Pin(pinned.clone()),
        })
        .await
        .expect("pin");
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let original = ArchivedMessage {
        id: target.id.clone(),
        body: Some("private preview".to_owned()),
        message_type: XmppMessageType::Groupchat,
        ..ArchivedMessage::for_test(
            Jid::from(room.with_resource_str("alice").expect("nick")),
            Jid::from(room.clone()),
        )
    };
    mam.store_message(&room, &original)
        .await
        .expect("target archive");
    let mut request = message(&room, &alice);
    request.from = Some(Jid::from(room.with_resource_str("alice").expect("nick")));
    let request_id = StanzaId::new("retraction-request", Jid::from(room.clone()));
    waddle_xmpp_core::xep0359::add_stanza_id(&mut request, &request_id);
    let tombstoned_request = ArchivedMessage {
        id: request_id.id.clone(),
        body: None,
        rich: Some(ArchivedRichMessage {
            payload: Some(ArchivedRichPayload::Tombstone(ArchivedTombstone {
                retraction_id: RichMessageId::new("later-retraction"),
                stamp: chrono::Utc::now(),
                moderation: None,
                sender_scope: Some(alice.to_bare()),
            })),
            ..Default::default()
        }),
        ..original.clone()
    };
    mam.store_message(&room, &tombstoned_request)
        .await
        .expect("tombstoned request");
    let (alice_tx, mut alice_rx) = tokio::sync::mpsc::channel(8);
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    register_test_connection(state.as_ref(), &alice, alice_tx).await;
    register_test_connection(state.as_ref(), &bob, bob_tx).await;
    let sink = PlanSink::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
        state.as_ref(),
        None,
    );
    deps.effects = &sink;
    deps.mam_storage = Some(&mam);
    deps.ingress_effect_capture = Some(capture.clone());
    super::super::effects::direct::external(
        &deps,
        ExternalDirectEffect::LinkPreviewRefs {
            mutations: vec![waddle_xmpp::ingress::LinkPreviewMediaRefMutation {
                upload_slot_id: uuid::Uuid::nil(),
                archive: room.clone(),
                message_id: RichMessageId::new(target.id.clone()).expect("message id"),
                current_archive_stanza_id: target.clone(),
                state: waddle_xmpp::ingress::LinkPreviewMediaRefState::Current,
            }],
        },
    );
    let unrelated_target = StanzaId::new("unrelated-target", Jid::from(room.clone()));
    interpret(
        vec![
            OutboundEvent::ApplyGroupchatRetractionTombstone {
                room: room.clone(),
                target_message_id: target.id.clone(),
                retraction_message: Box::new(request),
            },
            OutboundEvent::ApplyPinChange {
                room: room.clone(),
                request: PinChangeRequest::Pin {
                    target_stanza_id: unrelated_target.clone(),
                    pinner_jid: alice.to_bare(),
                    pinner_nick: "alice".to_owned(),
                    pinned_at: chrono::Utc::now(),
                },
            },
        ],
        &deps,
    )
    .await;
    let plan = sink.snapshot();
    assert!(plan.iter().any(|effect| matches!(&effect.effect,
        Effect::Durable(DurableEffect::Direct(DurableDirectEffect::RetractionTombstone { target: planned, .. })) if planned == &target)));
    let unpin = plan.iter().find(|effect| matches!(&effect.effect,
        Effect::External(ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
            mutation: RoomActorMutation::ApplyPin { change: PinStateChange::Unpin { target_stanza_id }, .. }, ..
        })) if target_stanza_id == &target)).expect("unpin mutation");
    assert_eq!(unpin.tombstone_suppression, PlanSuppressionPolicy::Always);
    let unrelated_pin = plan
        .iter()
        .find(|effect| {
            matches!(&effect.effect,
        Effect::External(ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
            mutation: RoomActorMutation::ApplyPin { change: PinStateChange::Pin(entry), .. }, ..
        })) if entry.target_stanza_id == unrelated_target)
        })
        .expect("unrelated pin");
    assert_eq!(
        unrelated_pin.tombstone_suppression,
        PlanSuppressionPolicy::TombstoneSwallowed
    );
    let system_archive = plan
        .iter()
        .find_map(|effect| match &effect.effect {
            Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
                message,
                ..
            })) if effect.tombstone_suppression == PlanSuppressionPolicy::Always => Some(message),
            _ => None,
        })
        .expect("durable system-message archive");
    assert_eq!(system_archive.from, Jid::from(room.clone()));
    assert!(capture.snapshot().intents.iter().any(|intent| matches!(intent,
        IngressEffectIntent::ArchiveAuthoritative { archive, stanza_id, archived_at, .. }
        if archive == &room && stanza_id.id == system_archive.id && archived_at == &system_archive.timestamp)));
    for recipient in [&alice, &bob] {
        let delivery = plan
            .iter()
            .find_map(|effect| match &effect.effect {
                Effect::External(ExternalEffect::Delivery(
                    ExternalDeliveryEffect::RouteToPeer { jid, stanza, .. },
                )) if jid == recipient
                    && effect.tombstone_suppression == PlanSuppressionPolicy::Always =>
                {
                    Some(stanza)
                }
                _ => None,
            })
            .expect("frozen system delivery");
        let Stanza::Message(message) = delivery.as_ref() else {
            panic!("message");
        };
        assert_eq!(message.to.as_ref(), Some(&Jid::from(recipient.clone())));
        assert_eq!(
            super::super::groupchat_archive::extract_room_stanza_id(message, &room).as_deref(),
            Some(system_archive.id.as_str())
        );
    }
    assert!(alice_rx.try_recv().is_err());
    assert!(bob_rx.try_recv().is_err());
    assert_eq!(actor.ask(GetPinList).await.expect("pins"), vec![pinned]);
    assert!(mam
        .get_message(&system_archive.id)
        .await
        .expect("archive lookup")
        .is_none());
    assert!(mam
        .get_message(&target.id)
        .await
        .expect("target")
        .expect("original")
        .rich
        .is_none());
    let clear = plan.iter().find(|effect| matches!(&effect.effect,
        Effect::External(ExternalEffect::Direct(ExternalDirectEffect::ClearLinkPreviewRefs { mutations }))
        if mutations.iter().any(|mutation| mutation.current_archive_stanza_id == target
            && mutation.state == waddle_xmpp::ingress::LinkPreviewMediaRefState::Unreferenced)))
        .expect("preview clear");
    assert_eq!(clear.tombstone_suppression, PlanSuppressionPolicy::Always);
}
