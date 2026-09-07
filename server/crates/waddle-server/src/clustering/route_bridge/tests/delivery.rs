use super::*;
use crate::room_effect_outbox::drain::drain_due_effects;
use crate::room_effect_outbox::{
    RoomEffectEnqueue, RoomEffectKey, RoomEffectLastError, RoomEffectOriginInstanceId,
    RoomEffectProducingNode,
};
use crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering;
use waddle_xmpp::muc::room_actor::GetSnapshot;
use waddle_xmpp::muc::room_registry_actor::{CreateInstantRoom, CreateRoom, GetRoom};
use waddle_xmpp::muc::{
    MucConfigStatusCode, RoomConfig, RoomLifecycleId, RoomLifecycleState, RoomMutationEffects,
    RoomRevision,
};
use waddle_xmpp::ownership::ClaimEpoch;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
use xmpp_parsers::message::Message;

fn relayed_muc_presence(
    sender: &jid::FullJid,
    room: &jid::BareJid,
    nick: &str,
    type_: xmpp_parsers::presence::Type,
) -> xmpp_parsers::presence::Presence {
    let mut presence = xmpp_parsers::presence::Presence::new(type_);
    presence.from = Some(jid::Jid::from(sender.clone()));
    presence.to = Some(jid::Jid::from(
        room.clone()
            .with_resource_str(nick)
            .expect("valid occupant JID"),
    ));
    presence
}

#[tokio::test]
async fn reserved_relay_join_stores_the_source_connection_generation() {
    let state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles::default(),
        Arc::new(InMemorySmSessionRegistry::new()),
    )
    .await;
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.web_socket_state = Arc::downgrade(&state);
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room JID");
    let sender: jid::FullJid = "alice@example.com/phone".parse().expect("full JID");
    let generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
    state
        .deps
        .protocol
        .room_registry
        .ask(CreateInstantRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("create room");
    let presence =
        relayed_muc_presence(&sender, &room, "alice", xmpp_parsers::presence::Type::None);

    deliver_reserved_muc_proxy(
        &services,
        &room,
        OrderedRelayMucProxyKind::JoinPresence,
        MucProxyOrigin::Connection(generation),
        &Stanza::Presence(presence),
        None,
        &mut None,
    )
    .await
    .expect("relayed join delivered");

    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("room lookup")
        .expect("room actor");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(snapshot.room.session_generation(&sender), Some(generation));
}

#[tokio::test]
async fn reserved_relay_unavailable_cannot_remove_a_replacement_generation() {
    let state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles::default(),
        Arc::new(InMemorySmSessionRegistry::new()),
    )
    .await;
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.web_socket_state = Arc::downgrade(&state);
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room JID");
    let sender: jid::FullJid = "alice@example.com/phone".parse().expect("full JID");
    let first = waddle_xmpp_core::OccupancySessionGeneration::mint();
    let replacement = waddle_xmpp_core::OccupancySessionGeneration::mint();
    state
        .deps
        .protocol
        .room_registry
        .ask(CreateInstantRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("create room");
    let available =
        relayed_muc_presence(&sender, &room, "alice", xmpp_parsers::presence::Type::None);

    for generation in [first, replacement] {
        deliver_reserved_muc_proxy(
            &services,
            &room,
            OrderedRelayMucProxyKind::JoinPresence,
            MucProxyOrigin::Connection(generation),
            &Stanza::Presence(available.clone()),
            None,
            &mut None,
        )
        .await
        .expect("relayed join delivered");
    }
    let unavailable = relayed_muc_presence(
        &sender,
        &room,
        "alice",
        xmpp_parsers::presence::Type::Unavailable,
    );
    deliver_reserved_muc_proxy(
        &services,
        &room,
        OrderedRelayMucProxyKind::OccupantPresence,
        MucProxyOrigin::Connection(first),
        &Stanza::Presence(unavailable),
        None,
        &mut None,
    )
    .await
    .expect("superseded relayed leave is terminal");

    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("room lookup")
        .expect("replacement keeps room actor alive");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    assert_eq!(snapshot.room.occupant_count(), 1);
    assert_eq!(snapshot.room.session_generation(&sender), Some(replacement));
}

#[test]
fn full_jid_bridge_rejects_groupchat_payloads() {
    let target = target_full();
    let mut message = Message::new(Some(jid::Jid::from(target.clone())));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;

    assert!(payload_for_recipient(jid::Jid::from(target), &Stanza::Message(message)).is_none());
}
#[tokio::test]
async fn unwired_bridge_reports_unreachable_without_advancing_effects() {
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );

    let err = bridge
        .deliver_reserved(&envelope(), &mut None)
        .await
        .expect_err("unwired bridge cannot deliver");

    assert_eq!(err, OrderedRelayNackReason::Unreachable);
}

#[tokio::test]
async fn remote_socket_delivery_preserves_direct_frame_kind() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));
    let target = target_full();
    let (tx, mut rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), entry);
    bridge
        .test_insert_remote_socket_registration(
            target.clone(),
            Arc::clone(&owner),
            NodeId::new("remote-user-owner".to_owned()),
        )
        .await;
    let registration_id = bridge
        .remote_socket_resources
        .lock()
        .await
        .get(&target)
        .expect("socket registration")
        .registration_id;

    let reply = bridge
        .deliver_remote_resource_frame_on_socket(RelayDeliverRemoteResourceFrame {
            frame: RemoteResourceOutboundFrame {
                jid: target.clone(),
                registration_id,
                stanza: RemoteStanza(Stanza::Message(Message::new(Some(jid::Jid::from(
                    target.clone(),
                ))))),
                kind: DeliveryKind::DirectFrame,
            },
            trace: RelayTraceContext::default(),
        })
        .await;

    assert_eq!(reply.status, RelayRemoteResourceFrameStatus::Delivered);
    let outbound = rx.recv().await.expect("socket receives relayed frame");
    assert_eq!(
        outbound.kind,
        DeliveryKind::DirectFrame,
        "remote resource frames must bypass the recipient pass"
    );
    assert!(
        outbound.write_acceptance.is_none(),
        "remote_resource_frame.v1 must remain enqueue-only"
    );
}

#[tokio::test]
async fn remote_socket_write_accepted_waits_for_writer_handoff() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));
    let target = target_full();
    let (tx, mut rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), entry);
    bridge
        .test_insert_remote_socket_registration(
            target.clone(),
            Arc::clone(&owner),
            NodeId::new("remote-user-owner".to_owned()),
        )
        .await;
    let registration = bridge
        .remote_socket_resources
        .lock()
        .await
        .get(&target)
        .cloned()
        .expect("socket registration");

    let bridge_for_task = Arc::clone(&bridge);
    let target_for_task = target.clone();
    let mut reply = tokio::spawn(async move {
        bridge_for_task
            .deliver_remote_resource_write_accepted_frame_on_socket(
                RelayDeliverRemoteResourceWriteAcceptedFrame {
                    frame: RemoteResourceWriteAcceptedOutboundFrame {
                        jid: target_for_task.clone(),
                        registration_id: registration.registration_id,
                        socket_generation: registration.socket_generation,
                        stanza: RemoteStanza(Stanza::Message(Message::new(Some(jid::Jid::from(
                            target_for_task,
                        ))))),
                    },
                    trace: RelayTraceContext::default(),
                },
            )
            .await
    });

    let outbound = rx
        .recv()
        .await
        .expect("destination dequeues write-accepted frame");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut reply)
            .await
            .is_err(),
        "reply must stay pending until the writer accepts the frame"
    );
    outbound
        .write_acceptance
        .as_ref()
        .expect("write acceptance token")
        .acknowledge();
    assert_eq!(
        reply.await.expect("reply task").status,
        RelayRemoteResourceWriteAcceptedStatus::WriteAccepted
    );
    assert!(
        matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "the handler must enqueue exactly one outbound frame"
    );
}

#[tokio::test]
async fn remote_socket_write_accepted_reports_acceptance_closed_when_writer_drops_frame() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));
    let target = target_full();
    let (tx, mut rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), entry);
    bridge
        .test_insert_remote_socket_registration(
            target.clone(),
            Arc::clone(&owner),
            NodeId::new("remote-user-owner".to_owned()),
        )
        .await;
    let registration = bridge
        .remote_socket_resources
        .lock()
        .await
        .get(&target)
        .cloned()
        .expect("socket registration");

    let bridge_for_task = Arc::clone(&bridge);
    let target_for_task = target.clone();
    let reply = tokio::spawn(async move {
        bridge_for_task
            .deliver_remote_resource_write_accepted_frame_on_socket(
                RelayDeliverRemoteResourceWriteAcceptedFrame {
                    frame: RemoteResourceWriteAcceptedOutboundFrame {
                        jid: target_for_task.clone(),
                        registration_id: registration.registration_id,
                        socket_generation: registration.socket_generation,
                        stanza: RemoteStanza(Stanza::Message(Message::new(Some(jid::Jid::from(
                            target_for_task,
                        ))))),
                    },
                    trace: RelayTraceContext::default(),
                },
            )
            .await
    });

    let outbound = rx
        .recv()
        .await
        .expect("destination dequeues write-accepted frame");
    assert!(outbound.write_acceptance.is_some());
    drop(outbound);

    assert_eq!(
        reply.await.expect("reply task").status,
        RelayRemoteResourceWriteAcceptedStatus::AcceptanceClosed
    );
}

#[tokio::test(start_paused = true)]
async fn remote_socket_write_accepted_reports_acceptance_pending_when_writer_stalls() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));
    let target = target_full();
    let (tx, mut rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), entry);
    bridge
        .test_insert_remote_socket_registration(
            target.clone(),
            Arc::clone(&owner),
            NodeId::new("remote-user-owner".to_owned()),
        )
        .await;
    let registration = bridge
        .remote_socket_resources
        .lock()
        .await
        .get(&target)
        .cloned()
        .expect("socket registration");

    let bridge_for_task = Arc::clone(&bridge);
    let target_for_task = target.clone();
    let reply = tokio::spawn(async move {
        bridge_for_task
            .deliver_remote_resource_write_accepted_frame_on_socket(
                RelayDeliverRemoteResourceWriteAcceptedFrame {
                    frame: RemoteResourceWriteAcceptedOutboundFrame {
                        jid: target_for_task.clone(),
                        registration_id: registration.registration_id,
                        socket_generation: registration.socket_generation,
                        stanza: RemoteStanza(Stanza::Message(Message::new(Some(jid::Jid::from(
                            target_for_task,
                        ))))),
                    },
                    trace: RelayTraceContext::default(),
                },
            )
            .await
    });

    let held = rx
        .recv()
        .await
        .expect("destination dequeues write-accepted frame");
    assert!(held.write_acceptance.is_some());
    tokio::time::advance(
        bridge.remote_resource_write_accepted_acceptance_timeout() + Duration::from_millis(1),
    )
    .await;

    assert_eq!(
        reply.await.expect("reply task").status,
        RelayRemoteResourceWriteAcceptedStatus::AcceptancePending
    );
    drop(held);
}

#[tokio::test]
async fn remote_socket_write_accepted_rejects_stale_socket_generation() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));
    let target = target_full();
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), entry);
    bridge
        .test_insert_remote_socket_registration(
            target.clone(),
            Arc::clone(&owner),
            NodeId::new("remote-user-owner".to_owned()),
        )
        .await;
    let registration = bridge
        .remote_socket_resources
        .lock()
        .await
        .get(&target)
        .cloned()
        .expect("socket registration");

    let reply = bridge
        .deliver_remote_resource_write_accepted_frame_on_socket(
            RelayDeliverRemoteResourceWriteAcceptedFrame {
                frame: RemoteResourceWriteAcceptedOutboundFrame {
                    jid: target.clone(),
                    registration_id: registration.registration_id,
                    socket_generation: RemoteResourceSocketGeneration::next(Some(
                        registration.socket_generation,
                    )),
                    stanza: RemoteStanza(Stanza::Message(Message::new(Some(jid::Jid::from(
                        target,
                    ))))),
                },
                trace: RelayTraceContext::default(),
            },
        )
        .await;

    assert_eq!(
        reply.status,
        RelayRemoteResourceWriteAcceptedStatus::StaleRegistration
    );
}

#[tokio::test]
async fn remote_socket_write_accepted_reports_unavailable_for_absent_resource() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));
    let target = target_full();

    let reply = bridge
        .deliver_remote_resource_write_accepted_frame_on_socket(
            RelayDeliverRemoteResourceWriteAcceptedFrame {
                frame: RemoteResourceWriteAcceptedOutboundFrame {
                    jid: target.clone(),
                    registration_id: RemoteResourceRegistrationId::fresh(),
                    socket_generation: RemoteResourceSocketGeneration::next(None),
                    stanza: RemoteStanza(Stanza::Message(Message::new(Some(jid::Jid::from(
                        target,
                    ))))),
                },
                trace: RelayTraceContext::default(),
            },
        )
        .await;

    assert_eq!(
        reply.status,
        RelayRemoteResourceWriteAcceptedStatus::Unavailable
    );
}

#[test]
fn write_accepted_status_classification_is_pinned() {
    use crate::clustering::route_bridge::delivery::remote_socket::classify_write_accepted_status;
    assert_eq!(
        classify_write_accepted_status(RelayRemoteResourceWriteAcceptedStatus::WriteAccepted),
        RegisteredRemoteWriteAcceptedDelivery::Delivered
    );
    assert_eq!(
        classify_write_accepted_status(RelayRemoteResourceWriteAcceptedStatus::AcceptanceClosed),
        RegisteredRemoteWriteAcceptedDelivery::Retryable
    );
    assert_eq!(
        classify_write_accepted_status(RelayRemoteResourceWriteAcceptedStatus::AcceptancePending),
        RegisteredRemoteWriteAcceptedDelivery::Retryable
    );
    // StaleRegistration maps to RefreshNeeded, whose ONLY consumer arm
    // classifies Retryable without settling the row or touching the owner
    // mirror — an owner-side refresh is structurally impossible.
    assert_eq!(
        classify_write_accepted_status(RelayRemoteResourceWriteAcceptedStatus::StaleRegistration),
        RegisteredRemoteWriteAcceptedDelivery::RefreshNeeded
    );
    assert_eq!(
        classify_write_accepted_status(RelayRemoteResourceWriteAcceptedStatus::Unavailable),
        RegisteredRemoteWriteAcceptedDelivery::Absent
    );
}

#[tokio::test]
async fn unreachable_write_accepted_ask_is_retryable_and_keeps_the_owner_mirror() {
    // A StaleRegistration reply means the destination's socket lifecycle
    // moved. An owner-side refresh is structurally impossible (only the
    // socket node's own re-registration rebuilds the mirror), so the
    // delivery must classify as retryable WITHOUT touching the mirror —
    // falsely settling the durable row or tearing down a mirror the socket
    // node still converges would lose the effect or the route.
    let stop = CancellationToken::new();
    stop.cancel();
    let bridge = OrderedRelayDeliveryBridge::new(stop, &ClusteringMessagingConfig::default());
    let target = target_full();
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    let stale = RemoteOwnerRegistration {
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_node: NodeId::new("origin-node".to_owned()),
        socket_generation: RemoteResourceSocketGeneration::next(None),
        owner,
    };
    bridge
        .remote_owner_resources
        .lock()
        .await
        .insert(target.clone(), stale.clone());

    // The cancelled stop token makes the relay ask fail fast; the delivery
    // outcome for an unreachable/stale destination must be retryable.
    let outcome = bridge
        .try_deliver_registered_remote_resource_write_accepted(
            &target,
            &Stanza::Message(Message::new(Some(jid::Jid::from(target.clone())))),
        )
        .await;
    assert_eq!(outcome, RegisteredRemoteWriteAcceptedDelivery::Retryable);
    assert!(
        bridge
            .remote_owner_resources
            .lock()
            .await
            .contains_key(&target),
        "a retryable stale/unreachable delivery must keep the owner mirror for the next pass"
    );
}

#[tokio::test]
async fn drained_remote_direct_frame_retry_releases_room_effect_as_infrastructure_transient() {
    let stop = CancellationToken::new();
    stop.cancel();
    let bridge = OrderedRelayDeliveryBridge::new(stop, &ClusteringMessagingConfig::default());
    let target = target_full();
    bridge.remote_owner_resources.lock().await.insert(
        target.clone(),
        RemoteOwnerRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            socket_node: NodeId::new("unreachable-remote-socket".to_owned()),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            owner: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );
    let state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles {
            ordered_relay_delivery_bridge: Some(Arc::clone(&bridge)),
            ..crate::clustering::ClusteringHandles::default()
        },
        Arc::new(InMemorySmSessionRegistry::new()),
    )
    .await;
    let room_jid: jid::BareJid = "room@muc.example.com".parse().expect("room JID");
    let lifecycle = RoomLifecycleId::generate();
    state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "remote-effect-room".to_owned(),
            channel_id: "remote-effect-room".to_owned(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create owned room");
    let store = &state.deps.protocol.room_effect_outbox;
    let connection = store.database().guard().await.expect("database connection");
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (lifecycle_id TEXT NOT NULL, room_jid TEXT NOT NULL, revision BIGINT NOT NULL, state TEXT NOT NULL)",
            (),
        )
        .await
        .expect("create lifecycle table");
    connection
        .execute(
            "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
            crate::db_params![
                lifecycle.to_string(),
                room_jid.to_string(),
                RoomRevision::initial().as_i64(),
                RoomLifecycleState::Active.as_db_str(),
            ],
        )
        .await
        .expect("insert lifecycle");
    drop(connection);
    let effects = RoomMutationEffects::config(
        room_jid,
        vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
        vec![target.clone()],
    );
    let origin = RoomEffectOriginInstanceId::new("test-origin".to_owned()).expect("origin");
    let producing_node =
        RoomEffectProducingNode::from_node_identity(NodeIdentity::new("node-a", "epoch-a"));
    let mut tx = store.database().begin().await.expect("transaction");
    let reservation = store
        .enqueue_in_tx(
            &mut tx,
            RoomEffectEnqueue {
                lifecycle,
                revision: RoomRevision::initial(),
                effects: &effects,
                origin: &origin,
                producing_node: &producing_node,
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue room effect");
    tx.commit().await.expect("commit room effect");
    store
        .arm_reservation(&reservation, 0)
        .await
        .expect("arm room effect");

    let summary = drain_due_effects(state.as_ref(), 0, 8)
        .await
        .expect("drain room effect");
    assert_eq!(summary.drained, 0);
    assert_eq!(summary.requeued, 1);
    let row = store
        .find(&RoomEffectKey {
            lifecycle,
            revision: RoomRevision::initial(),
            ordinal: reservation.ordinals[0],
        })
        .await
        .expect("find released row")
        .expect("retryable remote delivery retains the row");
    assert_eq!(
        row.last_error,
        Some(RoomEffectLastError::InfrastructureTransient)
    );
    assert_eq!(row.attempt_count, 1);
    assert!(row.lease_token.is_none());
}

#[tokio::test]
async fn receiver_rejects_target_claim_not_owned_by_this_node() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        other_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;

    let envelope = signed_envelope_for_services(&services, &keypair).await;
    let err = validate_claims(&services, &envelope)
        .await
        .expect_err("foreign target owner is not this relay");

    assert_eq!(
        err,
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target
        }
    );
}
#[tokio::test]
async fn receiver_accepts_fresh_origin_and_local_target_claims() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;

    let envelope = signed_envelope_for_services(&services, &keypair).await;
    validate_claims(&services, &envelope)
        .await
        .expect("claims match origin and receiver");
}
#[tokio::test]
async fn receiver_rejects_stale_sender_claim_before_delivery_effects() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let mut envelope = envelope_for_services(&services).await;
    envelope.sender_claim.epoch = ClaimEpoch(99);
    let envelope = sign_envelope(envelope, &keypair);

    let err = validate_claims(&services, &envelope)
        .await
        .expect_err("stale sender claim must be rejected");

    assert_eq!(
        err,
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Sender
        }
    );
}
#[tokio::test]
async fn bare_presence_direct_drops_blocked_sender_before_detached_replay() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

    let blocking = Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new());
    blocking.set_blocklist(target_bare(), vec![sender_full().to_bare()]);
    let services = services_with_claims_and_blocking(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
        blocking,
    )
    .await;

    let detached_stream = "stream-detached-presence";
    services
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: detached_stream.to_string(),
            user_id: target_bare().to_string(),
            jid: target_full(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached target resource");

    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.from = Some(jid::Jid::from(sender_full()));
    presence.to = Some(jid::Jid::from(target_bare()));

    deliver_reserved_bare_presence_direct(&services, &target_bare(), &Stanza::Presence(presence))
        .await
        .expect("blocked presence is silently dropped");

    let detached = services
        .sm_session_registry
        .peek_session(detached_stream)
        .await
        .expect("peek detached target resource")
        .expect("detached target resource remains");
    assert!(
        detached.unacked_stanzas.is_empty(),
        "blocked remote presence must not be queued for SM replay"
    );
}
#[tokio::test]
async fn receiver_nacks_iq_without_live_resource_instead_of_detached_queue() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let mut envelope = envelope_for_services(&services).await;
    envelope.payload = iq_payload();
    let envelope = sign_envelope(envelope, &keypair);
    bridge.wire(Arc::new(services));

    let err = bridge
        .deliver_reserved(&envelope, &mut None)
        .await
        .expect_err("remote full-JID IQ requires a live local resource");

    assert_eq!(err, OrderedRelayNackReason::TargetUnavailable);
}
#[tokio::test]
async fn receiver_delivers_full_jid_iq_as_peer_stanza() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target_full(),
            entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
        })
        .await
        .expect("register resource");
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let mut envelope = envelope_for_services(&services).await;
    envelope.payload = iq_payload();
    let envelope = sign_envelope(envelope, &keypair);
    bridge.wire(Arc::new(services));

    bridge
        .deliver_reserved(&envelope, &mut None)
        .await
        .expect("remote full-JID IQ delivers to live resource");

    let outbound = rx.try_recv().expect("queued outbound stanza");
    assert_eq!(
        outbound.kind,
        waddle_xmpp::registry::DeliveryKind::PeerStanza
    );
}
#[tokio::test]
async fn stale_registered_remote_resource_cleans_mirror_and_allows_local_fallback() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (tx, _rx) = mpsc::channel(1);
    let entry = ConnectionEntry::new(tx);
    let owner = entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry,
        })
        .await
        .expect("register remote mirror in user actor");

    let registration_id = RemoteResourceRegistrationId::fresh();
    bridge.remote_owner_resources.lock().await.insert(
        target.clone(),
        RemoteOwnerRegistration {
            registration_id,
            socket_node: NodeId::new("missing-socket-node".to_string()),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            owner: Arc::clone(&owner),
        },
    );

    let outcome = bridge
        .try_deliver_registered_remote_resource(
            &target,
            &Stanza::Message(Message::new(Some(jid::Jid::from(target.clone())))),
            DeliveryKind::PeerStanza,
        )
        .await;

    assert_eq!(outcome, None);
    assert!(
        !bridge
            .remote_owner_resources
            .lock()
            .await
            .contains_key(&target),
        "stale remote owner map entry must be removed"
    );
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &owner)
            .is_none(),
        "stale remote connection mirror must be removed so local fallback can run"
    );
}

#[tokio::test]
async fn remote_full_jid_route_reply_returns_detached_stream_identity() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = Arc::new(OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    ));
    bridge.wire(Arc::clone(&services));

    let source = sender_full();
    let (source_tx, _source_rx) = mpsc::channel(1);
    let source_entry = ConnectionEntry::new(source_tx);
    let source_owner = source_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(source.clone(), source_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: source.clone(),
            entry: source_entry,
        })
        .await
        .expect("register source mirror");

    let registration_id = RemoteResourceRegistrationId::fresh();
    let socket_generation = RemoteResourceSocketGeneration::next(None);
    bridge.remote_owner_resources.lock().await.insert(
        source.clone(),
        RemoteOwnerRegistration {
            registration_id,
            socket_node: NodeId::new("source-socket-node".to_string()),
            socket_generation,
            owner: Arc::clone(&source_owner),
        },
    );

    services
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "remote-direct-detached-stream".to_string(),
            user_id: target_bare().to_string(),
            jid: target_full(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached target");

    let mut message = Message::new(Some(jid::Jid::from(target_full())));
    message.from = Some(jid::Jid::from(source.clone()));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "remote detached".to_string(),
    );

    let reply = bridge
        .route_remote_resource_stanza_on_owner(
            RelayRouteRemoteResourceStanza {
                source_jid: source,
                registration_id,
                socket_generation,
                target: RemoteResourceRouteTarget::FullJid {
                    target: target_full(),
                    stanza: RemoteStanza(Stanza::Message(message)),
                },
                trace: RelayTraceContext::default(),
            },
            &mut None,
        )
        .await;

    assert_eq!(reply.outcome, RemoteResourceRouteOutcome::QueuedDetached);
    assert_eq!(
        reply.recipient_sm_append_streams,
        vec![SmSessionId::new("remote-direct-detached-stream")],
    );
}

pub(crate) async fn remote_carbon_owner_reply(
    source: jid::FullJid,
    sm: Arc<InMemorySmSessionRegistry>,
    before_fanout: impl std::future::Future<Output = ()>,
) -> RelayRemoteUserSideEffectReply {
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.sm_session_registry = sm;
    let services = Arc::new(services);
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));

    let detached_target = source
        .to_bare()
        .with_resource_str("carbon-sibling")
        .expect("target");
    let owner = source.to_bare();
    let (source_tx, _source_rx) = mpsc::channel(1);
    let source_entry = ConnectionEntry::new(source_tx);
    let source_owner = source_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(source.clone(), source_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: source.clone(),
            entry: source_entry,
        })
        .await
        .expect("register source mirror");

    let registration_id = RemoteResourceRegistrationId::fresh();
    let socket_generation = RemoteResourceSocketGeneration::next(None);
    bridge.remote_owner_resources.lock().await.insert(
        source.clone(),
        RemoteOwnerRegistration {
            registration_id,
            socket_node: NodeId::new("carbon-socket-node".to_string()),
            socket_generation,
            owner: Arc::clone(&source_owner),
        },
    );

    services
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "remote-carbon-detached-stream".to_string(),
            user_id: owner.to_string(),
            jid: detached_target.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached carbon target");

    let mut message = Message::new(Some(jid::Jid::from(target_full())));
    message.from = Some(jid::Jid::from(source.clone()));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "remote carbon".to_string(),
    );

    before_fanout.await;
    bridge
        .apply_remote_user_side_effect_on_owner(RelayRemoteUserSideEffect {
            source_jid: source.clone(),
            registration_id,
            socket_generation,
            effect: RemoteUserSideEffect::Carbons {
                owner: owner.clone(),
                message: RemoteStanza(Stanza::Message(message)),
                kind: RemoteCarbonKind::Sent,
                exclude: vec![source],
            },
            trace: RelayTraceContext::default(),
        })
        .await
}

#[tokio::test]
async fn remote_carbons_reply_returns_detached_stream_identity() {
    let reply = remote_carbon_owner_reply(
        "alice@example.com/web".parse().expect("source"),
        Arc::new(InMemorySmSessionRegistry::new()),
        async {},
    )
    .await;
    assert_eq!(reply.status, RelayRemoteUserSideEffectStatus::Applied);
    assert_eq!(
        reply.carbon_recipients,
        vec!["alice@example.com/carbon-sibling"
            .parse::<jid::FullJid>()
            .expect("target")]
    );
    assert_eq!(
        reply.recipient_sm_append_streams,
        vec![SmSessionId::new("remote-carbon-detached-stream")]
    );
}

#[tokio::test]
async fn stale_force_detach_error_cleans_old_socket_mirror() {
    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), old_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old remote mirror in user actor");

    let old_generation = RemoteResourceSocketGeneration::next(None);
    let registration = RemoteOwnerRegistration {
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_node: NodeId::new("missing-old-socket-node".to_string()),
        socket_generation: old_generation,
        owner: Arc::clone(&old_owner),
    };

    assert!(
        bridge
            .finish_remote_owner_registration_retire(
                &services,
                &target,
                &registration,
                Err(RelayAskError::NotFound {
                    node_id: registration.socket_node.clone(),
                }),
            )
            .await,
        "a missing old socket node proves no detach was committed and should permit cleanup"
    );
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &old_owner)
            .is_none(),
        "stale old owner mirror must be removed before replacement"
    );
    if let Some(actor) = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .await
        .expect("get user actor")
    {
        assert!(
            actor
                .ask(waddle_xmpp::registry::GetConnectionEntry {
                    jid: target.clone(),
                })
                .await
                .expect("get old actor mirror")
                .is_none(),
            "stale old user-actor mirror must be removed before replacement"
        );
    }
}

#[tokio::test]
async fn stale_force_detach_busy_actor_retries_and_cleans_without_janitor_work() {
    use tokio::sync::{oneshot, Notify};

    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), old_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old remote mirror in user actor");

    let actor = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .await
        .expect("get user actor")
        .expect("user actor exists");
    let entered = Arc::new(Notify::new());
    let (release_tx, release_rx) = oneshot::channel();
    actor
        .tell(
            waddle_xmpp::registry::user_actor::test_support::GateMailbox {
                entered: Arc::clone(&entered),
                release_rx,
            },
        )
        .await
        .expect("queue mailbox gate");
    entered.notified().await;

    let mut saw_mailbox_full = false;
    for _ in 0..128 {
        let sent = actor
            .tell(waddle_xmpp::registry::user_actor::test_support::MailboxNoop)
            .try_send();
        if matches!(sent, Err(kameo::error::SendError::MailboxFull(_))) {
            saw_mailbox_full = true;
            break;
        }
        sent.expect("mailbox filler should enqueue or report full");
    }
    assert!(
        saw_mailbox_full,
        "child mailbox must be busy before cleanup"
    );

    let old_generation = RemoteResourceSocketGeneration::next(None);
    let registration = RemoteOwnerRegistration {
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_node: NodeId::new("busy-old-socket-node".to_string()),
        socket_generation: old_generation,
        owner: Arc::clone(&old_owner),
    };

    let retire = tokio::spawn({
        let services = Arc::clone(&services);
        let bridge = Arc::clone(&bridge);
        let target = target.clone();
        let registration = registration.clone();
        async move {
            bridge
                .finish_remote_owner_registration_retire(
                    &services,
                    &target,
                    &registration,
                    Err(RelayAskError::NotFound {
                        node_id: registration.socket_node.clone(),
                    }),
                )
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    release_tx.send(()).expect("release mailbox gate");

    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), retire)
            .await
            .expect("bounded retry must not hang")
            .expect("retire task joins"),
        "a brief child-actor busy window should converge synchronously"
    );
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &old_owner)
            .is_none(),
        "stale old owner mirror must be removed before replacement"
    );
    assert_eq!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::user_registry::test_support::PendingUnregisterCount)
            .await
            .expect("pending unregister count"),
        0,
        "successful bounded retry must not leave janitor work behind"
    );
    assert!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: target.to_bare(),
            })
            .await
            .expect("get user actor after cleanup")
            .is_none(),
        "successful bounded retry prunes the now-empty user actor"
    );
}

#[tokio::test]
async fn stale_force_detach_persistently_busy_actor_records_janitor_retry() {
    use tokio::sync::{oneshot, Notify};

    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), old_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old remote mirror in user actor");

    let actor = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .await
        .expect("get user actor")
        .expect("user actor exists");
    let entered = Arc::new(Notify::new());
    let (release_tx, release_rx) = oneshot::channel();
    actor
        .tell(
            waddle_xmpp::registry::user_actor::test_support::GateMailbox {
                entered: Arc::clone(&entered),
                release_rx,
            },
        )
        .await
        .expect("queue mailbox gate");
    entered.notified().await;

    let mut saw_mailbox_full = false;
    for _ in 0..128 {
        let sent = actor
            .tell(waddle_xmpp::registry::user_actor::test_support::MailboxNoop)
            .try_send();
        if matches!(sent, Err(kameo::error::SendError::MailboxFull(_))) {
            saw_mailbox_full = true;
            break;
        }
        sent.expect("mailbox filler should enqueue or report full");
    }
    assert!(
        saw_mailbox_full,
        "child mailbox must be busy before cleanup"
    );

    let old_generation = RemoteResourceSocketGeneration::next(None);
    let registration = RemoteOwnerRegistration {
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_node: NodeId::new("stuck-old-socket-node".to_string()),
        socket_generation: old_generation,
        owner: Arc::clone(&old_owner),
    };
    bridge
        .remote_owner_resources
        .lock()
        .await
        .insert(target.clone(), registration.clone());

    bridge
        .cleanup_remote_owner_resource_if_registration(&target, registration.registration_id)
        .await;
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &old_owner)
            .is_none(),
        "stale old owner mirror must be removed immediately after recording retry work"
    );
    assert!(
        !bridge
            .remote_owner_resources
            .lock()
            .await
            .contains_key(&target),
        "cleanup must forget the stale owner-side tracking entry immediately"
    );
    assert_eq!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::user_registry::test_support::PendingUnregisterCount)
            .await
            .expect("pending unregister count"),
        1,
        "persistent busy cleanup must leave one exact janitor retry record"
    );

    release_tx.send(()).expect("release mailbox gate");
    tokio::task::yield_now().await;

    assert_eq!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::RetryUserRegistryConvergence)
            .await
            .expect("retry convergence"),
        (0, 0)
    );

    let (fresh_tx, _fresh_rx) = mpsc::channel(1);
    let fresh_entry = ConnectionEntry::new(fresh_tx);
    let successor = RelayRegisterRemoteUserResource {
        jid: target.clone(),
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_generation: RemoteResourceSocketGeneration::next(None),
        socket_node: NodeId::new("replacement-socket-node".to_string()),
        state: RemoteResourceStateSnapshot::from_entry(
            &fresh_entry,
            services.connection_registry.get_presence_state(&target),
        ),
        trace: RelayTraceContext::default(),
    };
    assert_eq!(
        bridge
            .register_remote_user_resource_on_owner(successor.clone())
            .await
            .status,
        RelayRemoteResourceRegistrationStatus::Registered,
        "a successor same-JID remote resource must register after janitor convergence"
    );
    assert!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: target.to_bare(),
            })
            .await
            .expect("get user actor after successor registration")
            .is_some(),
        "the successor registration should recreate the user actor cleanly"
    );
    let successor_registration = bridge
        .remote_owner_resources
        .lock()
        .await
        .get(&target)
        .cloned()
        .expect("successor owner registration");
    assert_eq!(
        successor_registration.registration_id, successor.registration_id,
        "the stale tracking entry must be replaced by the successor registration"
    );
    assert!(
        services
            .connection_registry
            .entry_if_owner(&target, &successor_registration.owner)
            .is_some(),
        "the successor registration must publish a fresh routing mirror"
    );
}

#[tokio::test]
async fn remote_owner_unregister_reply_reports_recorded_retry() {
    use tokio::sync::{oneshot, Notify};

    let services = Arc::new(
        services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await,
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    bridge.wire(Arc::clone(&services));

    let target = target_full();
    let (old_tx, _old_rx) = mpsc::channel(1);
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    services
        .connection_registry
        .register_entry(target.clone(), old_entry.clone());
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old remote mirror in user actor");

    let actor = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .await
        .expect("get user actor")
        .expect("user actor exists");
    let entered = Arc::new(Notify::new());
    let (release_tx, release_rx) = oneshot::channel();
    actor
        .tell(
            waddle_xmpp::registry::user_actor::test_support::GateMailbox {
                entered: Arc::clone(&entered),
                release_rx,
            },
        )
        .await
        .expect("queue mailbox gate");
    entered.notified().await;

    let mut saw_mailbox_full = false;
    for _ in 0..128 {
        let sent = actor
            .tell(waddle_xmpp::registry::user_actor::test_support::MailboxNoop)
            .try_send();
        if matches!(sent, Err(kameo::error::SendError::MailboxFull(_))) {
            saw_mailbox_full = true;
            break;
        }
        sent.expect("mailbox filler should enqueue or report full");
    }
    assert!(
        saw_mailbox_full,
        "child mailbox must be busy before cleanup"
    );

    let registration = RemoteOwnerRegistration {
        registration_id: RemoteResourceRegistrationId::fresh(),
        socket_node: NodeId::new("busy-socket-node".to_string()),
        socket_generation: RemoteResourceSocketGeneration::next(None),
        owner: Arc::clone(&old_owner),
    };
    bridge
        .remote_owner_resources
        .lock()
        .await
        .insert(target.clone(), registration.clone());

    let reply = bridge
        .unregister_remote_user_resource_on_owner(RelayUnregisterRemoteUserResource {
            jid: target.clone(),
            registration_id: registration.registration_id,
            socket_generation: registration.socket_generation,
            trace: RelayTraceContext::default(),
        })
        .await;

    assert_eq!(
        reply.status,
        RelayRemoteResourceUnregisterStatus::RecordedRetry
    );
    assert_eq!(
        services
            .user_registry
            .ask(waddle_xmpp::registry::user_registry::test_support::PendingUnregisterCount)
            .await
            .expect("pending unregister count"),
        1,
        "owner reply must only report RecordedRetry once the janitor obligation exists"
    );

    release_tx.send(()).expect("release mailbox gate");
}

#[tokio::test]
async fn receiver_nacks_dropped_peer_delivery_instead_of_acknowledging_loss() {
    let keypair = Keypair::generate_ed25519();
    let services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    tx.try_send(waddle_xmpp::registry::OutboundStanza::peer_stanza(
        Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
    ))
    .expect("seed outbound channel to capacity");
    services
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: target_full(),
            entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
        })
        .await
        .expect("register resource");
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let envelope = sign_envelope(envelope_for_services(&services).await, &keypair);
    bridge.wire(Arc::new(services));

    let err = bridge
        .deliver_reserved(&envelope, &mut None)
        .await
        .expect_err("full outbound channel must not be ACKed as delivered");

    assert_eq!(err, OrderedRelayNackReason::Backpressure);
}

#[tokio::test]
async fn reserved_groupchat_requires_committed_origin_admission() {
    let state = create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles::default(),
        Arc::new(InMemorySmSessionRegistry::new()),
    )
    .await;
    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        receiver_identity(),
        test_peer_id(),
    )
    .await;
    services.web_socket_state = Arc::downgrade(&state);
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let mut message = Message::new(Some(room.clone().into()));
    message.from = Some("alice@example.com/web".parse().expect("sender"));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    assert!(matches!(
        deliver_reserved_muc_proxy(
            &services,
            &room,
            OrderedRelayMucProxyKind::GroupchatMessage,
            MucProxyOrigin::Server,
            &Stanza::Message(message),
            None,
            &mut None
        )
        .await,
        Err(OrderedRelayNackReason::ParseFailure)
    ));
}
