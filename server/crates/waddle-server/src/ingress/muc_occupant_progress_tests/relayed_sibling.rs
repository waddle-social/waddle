use super::*;
use crate::server::routes::interpret::{effects::Effect, plan_muc_for_relay};
use crate::server::routes::interpret::{
    ControlledMucRelay, FullJidDeliveryOutcome, OrderedRelayRouteOrigin, CONTROLLED_MUC_RELAY,
};
use std::sync::{Arc, Mutex};
use waddle_xmpp::ownership::{
    ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::Stanza;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Destination {
    Live,
    Detached,
    Remote,
}

fn guard_plan(plan: &mut effects::IngressPlan, fence: &waddle_xmpp::muc::RoomClaimFenceContext) {
    if let effects::RoomExecutionPath::Local { fence: planned, .. } = &mut plan.room_execution {
        *planned = effects::room::RoomFenceRequirement::Guarded(fence.clone());
    }
    for planned in &mut plan.plan {
        if let Effect::Durable(effects::DurableEffect::Room(
            effects::room::DurableRoomEffect::ArchiveGroupchat { fence: planned, .. },
        )) = &mut planned.effect
        {
            *planned = effects::room::RoomFenceRequirement::Guarded(fence.clone());
        }
    }
}

async fn relayed_sibling_retry(mut fixture: IngressFixture, destination: Destination) {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let claims = Arc::new(InProcessClaimStore::new());
    let state = socket_tests::create_test_websocket_state_with_clustering(
        crate::clustering::ClusteringHandles {
            claim_store: (destination == Destination::Remote)
                .then(|| claims.clone() as Arc<dyn ClaimStore>),
            node_identity: (destination == Destination::Remote).then(|| {
                SharedNodeIdentity::new(NodeIdentity::new("ingress-owner", "ingress-incarnation"))
            }),
            ordered_relay_delivery_bridge: (destination == Destination::Remote).then(|| {
                crate::clustering::route_bridge::OrderedRelayDeliveryBridge::new(
                    tokio_util::sync::CancellationToken::new(),
                    &crate::config::ClusteringMessagingConfig::default(),
                )
            }),
            ..Default::default()
        },
        sm.clone(),
    )
    .await;
    let room: jid::BareJid = "relayed-sibling@muc.example.com".parse().expect("room");
    let mut submission = fixture.submission(Some("relayed-sibling-retry"), "frozen original body");
    let sender = submission.sender.clone();
    let sibling = sender
        .to_bare()
        .with_resource_str("mobile")
        .expect("sibling");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "relayed-sibling".into(),
            channel_id: "relayed-sibling".into(),
            config: Default::default(),
        })
        .await
        .expect("room");
    let mut receivers = Vec::new();
    for (resource, nick) in [(&sender, "original"), (&sibling, "mobile")] {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        if destination == Destination::Detached && resource == &sibling {
            super::local::store_detached(&sm, resource).await;
        } else {
            socket_tests::register_test_connection(&state, resource, tx).await;
        }
        receivers.push(rx);
        actor
            .ask(Join {
                nick: nick.into(),
                real_jid: resource.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
    }
    let mut message = submission.plan.sanitized_message.clone();
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.to = Some(room.clone().into());
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    submission.plan.sanitized_message = message.clone();
    let relay_target =
        waddle_xmpp::ingress::RelayTargetIdentity::owner_node("room-owner", "owner-epoch");
    let dispatch = IngressEffectIntent::DispatchToRoomRemote {
        room: room.clone(),
        relay_target: relay_target.clone(),
    };
    let dispatch_receipt = receipt_key(&dispatch).expect("dispatch receipt");
    submission.plan.plan.clear();
    submission.plan.intents = vec![dispatch];
    submission.plan.room_execution = effects::RoomExecutionPath::Remote {
        room: room.clone(),
        relay_target,
    };
    let origin = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("origin commit");
    let key = origin.message_key.expect("key");
    let fence = fixture.room_fence(&room).await;
    submission.identity = IngressStreamIdentity::Relayed {
        canonical: IngressCanonicalRef {
            message_key: key,
            sender_bare: sender.to_bare(),
            origin_id: submission.digest_input.origin().cloned(),
        },
        room: room.clone(),
        room_fence: fence.clone(),
    };
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    if destination == Destination::Remote {
        claims
            .acquire(
                &Entity::new(EntityType::UserActor, sender.to_bare().to_string()),
                &NodeIdentity::new("remote-owner", "remote-epoch"),
            )
            .await
            .expect("foreign sender resources claim");
        deps.ordered_relay_origin = Some(OrderedRelayRouteOrigin::room(&room));
    }
    submission.plan = plan_muc_for_relay(&deps, room.clone(), message.clone()).await;
    guard_plan(&mut submission.plan, &fence);
    let intent = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("MUC intent")
        .clone();
    let receipt = receipt_key(&intent).expect("MUC receipt");
    submission
        .plan
        .plan
        .sort_by_key(|planned| match &planned.effect {
            Effect::External(effect) => {
                super::super::recorded::single_target(effect) == Some(&sibling)
            }
            _ => false,
        });
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("owner commit");
    let stalled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let relay_targets = Arc::new(Mutex::new(Vec::new()));
    let first_execution = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::from_secs(1),
    );
    let first_report = if destination == Destination::Remote {
        CONTROLLED_MUC_RELAY
            .scope(
                (
                    ControlledMucRelay::Outcome(Some(FullJidDeliveryOutcome::Unavailable)),
                    relay_targets.clone(),
                ),
                first_execution,
            )
            .await
    } else {
        let report = STALL_DELIVERY_RESOURCE
            .scope((sibling.clone(), stalled.clone()), first_execution)
            .await;
        assert!(stalled.load(std::sync::atomic::Ordering::SeqCst));
        report
    };
    assert!(first_report.receipt_failures.is_empty(), "{first_report:?}");
    assert!(
        receivers[1].try_recv().is_err(),
        "sibling copy stays pending"
    );
    assert_eq!(
        first_report
            .frame_obligations
            .iter()
            .map(|frame| frame.frames.len())
            .sum::<usize>(),
        1,
        "original sender reflection travels in owner reply"
    );
    // Drop the owner reply before origin confirmation, losing the dispatch receipt.
    drop(first_report);
    let mut tx = fixture.uow.begin().await.expect("inspect first attempt");
    assert!(DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("progress")
        .is_empty());
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        key,
        dispatch_receipt.kind,
        &dispatch_receipt.semantic_identity_hash
    )
    .await
    .expect("lost dispatch receipt"));
    let frozen = crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("source")
        .expect("envelope");
    tx.commit().await.expect("read commit");
    let frozen_source =
        super::super::room_canonical::source(&frozen, &intent).expect("frozen source");

    submission.sender = sibling.clone();
    message.from = Some(sibling.clone().into());
    submission.plan = plan_muc_for_relay(&deps, room.clone(), message).await;
    guard_plan(&mut submission.plan, &fence);
    assert!(submission.plan.plan.iter().any(|planned| matches!(&planned.effect,
        Effect::External(ExternalEffect::Frame(stanza)) if matches!(stanza.as_ref(), Stanza::Message(copy) if copy.to == Some(sibling.clone().into())))), "real relay planner converts sibling reflection to Frame");
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("relayed sibling owner retry");
    assert_eq!(retry.message_key, Some(key));
    let mut reflections = 0;
    let mut repairs = 0;
    for (index, effect) in retry.external.iter().enumerate() {
        match effect {
            ExternalEffect::Frame(stanza) => {
                assert!(
                    !retry.external_receipts[index].contains(&receipt),
                    "frame never owns kind-2 receipt"
                );
                assert!(!super::super::execute_uow::owns(
                    effect,
                    &retry.route_progress
                ));
                if let Stanza::Message(copy) = stanza.as_ref() {
                    if copy.to == Some(sibling.clone().into()) {
                        reflections += 1;
                        assert_eq!(
                            copy.from,
                            Some(room.with_resource_str("mobile").expect("nick").into())
                        );
                    }
                }
            }
            ExternalEffect::Delivery(delivery)
                if super::super::recorded::single_target(effect) == Some(&sibling) =>
            {
                let stanza = match (destination, delivery) {
                    (Destination::Live, ExternalDeliveryEffect::RouteToPeer { stanza, .. })
                    | (
                        Destination::Detached,
                        ExternalDeliveryEffect::QueueDetached { stanza, .. },
                    )
                    | (Destination::Remote, ExternalDeliveryEffect::RelayFullJid { stanza, .. }) => {
                        stanza
                    }
                    _ => panic!(
                        "historical repair must use the occupant's resolved delivery destination"
                    ),
                };
                repairs += 1;
                assert!(retry.external_receipts[index].contains(&receipt));
                assert!(super::super::execute_uow::owns(
                    effect,
                    &retry.route_progress
                ));
                let Stanza::Message(copy) = stanza.as_ref() else {
                    panic!("message repair")
                };
                assert_eq!(copy.from, frozen_source.from);
                assert_eq!(copy.bodies, frozen_source.bodies);
                assert_eq!(copy.to, Some(sibling.clone().into()));
                let IngressEffectIntent::RouteMucGroupchat { route_identity, .. } = &intent else {
                    panic!("MUC intent")
                };
                assert!(super::super::receipts::routing::message_identity(
                    copy,
                    route_identity
                ));
                assert_eq!(
                    waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(copy),
                    waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(frozen_source)
                );
            }
            _ => {}
        }
    }
    assert_eq!(reflections, 1, "fresh sibling reflection retained");
    assert_eq!(
        repairs, 1,
        "relayed reflection must clone a progress-owned frozen sibling delivery"
    );
    relay_targets.lock().expect("relay targets").clear();
    let report = CONTROLLED_MUC_RELAY
        .scope(
            (
                ControlledMucRelay::Outcome(Some(FullJidDeliveryOutcome::Delivered)),
                relay_targets.clone(),
            ),
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &retry,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            ),
        )
        .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    match destination {
        Destination::Live => assert!(
            receivers[1].try_recv().is_ok(),
            "historical copy reaches sibling"
        ),
        Destination::Detached => {
            assert_eq!(
                super::local::append_count(&sm, &sibling).await,
                1,
                "frozen repair queued for detached sibling"
            );
            let append_key = waddle_xmpp::stream_management::SmIngressAppendKey {
                message_key: key,
                kind: waddle_xmpp::stream_management::SmIngressReceiptKind::from_storage(
                    receipt.kind.to_storage(),
                ),
                semantic_identity_hash: receipt.semantic_identity_hash,
                resource: sibling.clone(),
            };
            assert!(
                crate::sm_persistence::ingress_append::get(&fixture.db, &append_key)
                    .await
                    .expect("append ledger")
                    .is_some(),
                "detached copy carries MUC key and sibling resource"
            );
        }
        Destination::Remote => assert_eq!(
            *relay_targets.lock().expect("relay targets"),
            vec![sibling.clone()],
            "remote repair delivered on exact resource channel"
        ),
    }
    assert!(
        receivers[1].try_recv().is_err(),
        "only one historical delivery"
    );
    assert_eq!(report.frame_obligations.iter().flat_map(|frame| &frame.frames).filter(|stanza| matches!(stanza, Stanza::Message(copy) if copy.to == Some(sibling.clone().into()))).count(), 1, "fresh reflection returned alongside repaired delivery");
    assert!(report
        .frame_obligations
        .iter()
        .all(|frame| !frame.receipt_keys.contains(&receipt)));
    let mut tx = fixture.uow.begin().await.expect("inspect repair");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("progress"),
        vec![sibling]
    );
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("MUC aggregate settled"));
    tx.commit().await.expect("read commit");
    // The successful retry reply can now settle the origin's independent dispatch.
    EffectReceiptRepository::record_receipt_pooled(
        &fixture.db,
        key,
        dispatch_receipt.kind,
        &dispatch_receipt.semantic_identity_hash,
    )
    .await
    .expect("confirm origin dispatch");
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("complete fanout"));
    fixture.close().await;
}

#[tokio::test]
async fn postgres_muc_occupant_progress_relayed_sibling_reflection() {
    if let Some(fixture) = IngressFixture::postgres("relayed_sibling_reflection").await {
        relayed_sibling_retry(fixture, Destination::Live).await;
    }
}

#[tokio::test]
async fn postgres_muc_occupant_progress_relayed_sibling_detached_reflection() {
    if let Some(fixture) = IngressFixture::postgres("relayed_sibling_detached").await {
        relayed_sibling_retry(fixture, Destination::Detached).await;
    }
}

#[tokio::test]
async fn postgres_muc_occupant_progress_relayed_sibling_remote_reflection() {
    if let Some(fixture) = IngressFixture::postgres("relayed_sibling_remote").await {
        relayed_sibling_retry(fixture, Destination::Remote).await;
    }
}
