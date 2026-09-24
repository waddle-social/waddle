//! Owner-returned reflections must not overtake accepted occupant copies.
use super::*;
use crate::server::routes::interpret::{effects::Effect, plan_muc_for_relay};
use waddle_xmpp::Stanza;

fn groupchat_submission(
    fixture: &IngressFixture,
    room: &jid::BareJid,
    sender: &jid::FullJid,
    origin: &str,
    body: &str,
) -> IngressSubmission {
    let mut submission = fixture.submission(Some(origin), body);
    submission.sender = sender.clone();
    submission.target = NormalizedTarget::Bare(room.clone());
    let message = &mut submission.plan.sanitized_message;
    message.from = Some(sender.clone().into());
    message.to = Some(room.clone().into());
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.digest_input = DigestInput::from_parsed(
        message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("room digest");
    submission
}

fn guard_plan(plan: &mut IngressPlan, fence: &waddle_xmpp::muc::RoomClaimFenceContext) {
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

async fn receipted(
    fixture: &IngressFixture,
    key: waddle_xmpp::ingress::MessageKey,
    receipt: &EffectReceiptKey,
) -> bool {
    let mut tx = fixture.uow.begin().await.expect("receipt transaction");
    let found = EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("reflection receipt");
    tx.commit().await.expect("receipt read");
    found
}

fn body(outbound: waddle_xmpp::registry::OutboundStanza) -> String {
    let Stanza::Message(message) = outbound.stanza else {
        panic!("room message")
    };
    message.bodies.values().next().expect("body").clone()
}

async fn queued_copy_before_relayed_reflection(mut fixture: IngressFixture, full: bool) {
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "reflection-order@muc.example.com".parse().expect("room");
    let sender = fixture.submission(None, "").sender;
    let other = sender
        .to_bare()
        .with_resource_str("other")
        .expect("other occupant");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "reflection-order".into(),
            channel_id: "reflection-order".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    let (tx, mut receiver) = tokio::sync::mpsc::channel(if full { 1 } else { 4 });
    socket_tests::register_test_connection(&state, &sender, tx).await;
    let (tx, mut other_receiver) = tokio::sync::mpsc::channel(4);
    socket_tests::register_test_connection(&state, &other, tx).await;
    for (resource, nick) in [(&sender, "sender"), (&other, "other")] {
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
    let fence = fixture.room_fence(&room).await;
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    let mut older = groupchat_submission(&fixture, &room, &other, "older", "A");
    let message = older.plan.sanitized_message.clone();
    plan_broadcast(&mut older, &room, &message, &deps).await;
    guard_plan(&mut older.plan, &fence);
    let older = commit_submission(&fixture.uow, &older, 1)
        .await
        .expect("commit A");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &older,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(report.frame_obligations.is_empty());
    assert_eq!(
        body(other_receiver.try_recv().expect("A sender reflection")),
        "A"
    );
    let mut tx = fixture.uow.begin().await.expect("A progress");
    let occupant_receipt = &older
        .route_progress
        .iter()
        .find(|progress| !progress.is_direct())
        .expect("A fanout")
        .receipt;
    assert!(DeliveryProgressRepository::load(
        &mut tx,
        older.message_key.expect("A key"),
        occupant_receipt
    )
    .await
    .expect("A accepted")
    .contains(&sender));
    tx.commit().await.expect("A progress read");

    // The origin begins B while A is still in its socket's outbound queue.
    let mut newer = groupchat_submission(&fixture, &room, &sender, "newer", "B");
    let message = newer.plan.sanitized_message.clone();
    let relay_target =
        waddle_xmpp::ingress::RelayTargetIdentity::owner_node("room-owner", "owner-epoch");
    newer.plan.intents = vec![IngressEffectIntent::DispatchToRoomRemote {
        room: room.clone(),
        relay_target: relay_target.clone(),
    }];
    newer.plan.room_execution = effects::RoomExecutionPath::Remote {
        room: room.clone(),
        relay_target,
    };
    let origin = commit_submission(&fixture.uow, &newer, 1)
        .await
        .expect("B origin commit");
    let key = origin.message_key.expect("B key");
    newer.identity = IngressStreamIdentity::Relayed {
        canonical: IngressCanonicalRef {
            message_key: key,
            sender_bare: sender.to_bare(),
            origin_id: newer.digest_input.origin().cloned(),
        },
        room: room.clone(),
        room_fence: fence.clone(),
    };
    newer.plan = plan_muc_for_relay(&deps, room.clone(), message).await;
    guard_plan(&mut newer.plan, &fence);
    let newer = commit_submission(&fixture.uow, &newer, 1)
        .await
        .expect("B owner commit");
    let reflection = &newer
        .route_progress
        .iter()
        .find(|progress| progress.reflection_room.is_some())
        .expect("independent reflection")
        .receipt;
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &newer,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(
        report.frame_obligations.is_empty(),
        "B must not bypass queued A through the response writer"
    );
    assert_eq!(receipted(&fixture, key, reflection).await, !full);
    let oldest = receiver.try_recv().expect("oldest socket copy");
    let older_position = oldest
        .ingress_append
        .as_ref()
        .expect("A append authority")
        .archive_positions
        .iter()
        .find(|position| position.archive == room)
        .expect("A archive position")
        .ordinal;
    assert_eq!(body(oldest), "A");
    if full {
        assert!(
            receiver.try_recv().is_err(),
            "full queue leaves B unresolved"
        );
        deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
        super::super::recovery_executor::recover_row(
            &fixture.db,
            &fixture.uow,
            &deps,
            key,
            tokio::time::Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("recover reflection after queue drains");
        assert!(
            receipted(&fixture, key, reflection).await,
            "recovery settles the accepted reflection"
        );
    }
    let reflected = receiver.try_recv().expect("newer reflection");
    let newer_position = reflected
        .ingress_append
        .as_ref()
        .expect("B reflection authority")
        .archive_positions
        .iter()
        .find(|position| position.archive == room)
        .expect("B archive position")
        .ordinal;
    assert!(
        older_position < newer_position,
        "resource FIFO follows committed archive order"
    );
    assert_eq!(body(reflected), "B");
    assert!(receiver.try_recv().is_err(), "one canonical reflection");
    assert_eq!(
        body(other_receiver.try_recv().expect("B occupant copy")),
        "B"
    );
    assert!(
        other_receiver.try_recv().is_err(),
        "recovery never repeats completed occupant delivery"
    );
    fixture.close().await;
}

#[tokio::test]
async fn postgres_xep0045_relayed_reflection_follows_queued_archive_copy() {
    if let Some(fixture) = IngressFixture::postgres("reflection_fifo").await {
        queued_copy_before_relayed_reflection(fixture, false).await;
    }
}

#[tokio::test]
async fn postgres_xep0045_full_reflection_queue_retains_recoverable_receipt() {
    if let Some(fixture) = IngressFixture::postgres("reflection_fifo_full").await {
        queued_copy_before_relayed_reflection(fixture, true).await;
    }
}
