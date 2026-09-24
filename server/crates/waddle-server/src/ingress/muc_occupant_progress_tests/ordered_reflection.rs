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

async fn shared_state(
    fixture: &IngressFixture,
) -> std::sync::Arc<crate::server::routes::websocket::WebSocketState> {
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared database");
    socket_tests::create_test_websocket_state_with_db_pool_and_ingress(
        std::sync::Arc::new(pool),
        std::sync::Arc::new(fixture.authority().await),
    )
    .await
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

#[tokio::test]
async fn sqlite_xep0045_progress_contention_retries_without_repeating_live_delivery() {
    use crate::ingress::execute_uow::CONTEND_DELIVERY_PROGRESS_ONCE;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    let fixture = IngressFixture::sqlite().await;
    let state = shared_state(&fixture).await;
    let room: jid::BareJid = "progress-contention@muc.example.com".parse().expect("room");
    let sender = fixture.submission(None, "").sender;
    let other: jid::FullJid = "juliet@example.com/phone".parse().expect("other occupant");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "progress-contention".into(),
            channel_id: "progress-contention".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    let mut receivers = Vec::new();
    for (resource, nick) in [(&sender, "sender"), (&other, "other")] {
        let (tx, receiver) = tokio::sync::mpsc::channel(4);
        socket_tests::register_test_connection(&state, resource, tx).await;
        receivers.push(receiver);
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
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    let contention = Arc::new(AtomicBool::new(true));
    for (index, (author, text)) in [(&sender, "A"), (&other, "B")].into_iter().enumerate() {
        let mut submission = groupchat_submission(&fixture, &room, author, text, text);
        let message = submission.plan.sanitized_message.clone();
        plan_broadcast(&mut submission, &room, &message, &deps).await;
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("commit archived room message");
        assert!(
            !decision.archive_ids.is_empty(),
            "exercise archive ordering"
        );
        let execute = execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        );
        // Fail after inserting progress, so the retry must reopen a rolled-back
        // transaction without repeating the preceding live socket enqueue.
        let report = if index == 0 {
            CONTEND_DELIVERY_PROGRESS_ONCE
                .scope(Arc::clone(&contention), execute)
                .await
        } else {
            execute.await
        };
        assert!(!contention.load(Ordering::SeqCst), "fault was exercised");
        assert!(report.receipt_failures.is_empty(), "{report:?}");
        assert!(report.frame_obligations.is_empty(), "{report:?}");
        assert!(
            report
                .outcomes
                .iter()
                .all(|(_, outcome)| *outcome == ExternalOutcome::Done),
            "contention must settle before the next archived message: {report:?}"
        );
        let key = decision.message_key.expect("message key");
        let mut tx = fixture.uow.begin().await.expect("inspect progress");
        for progress in &decision.route_progress {
            assert_eq!(
                DeliveryProgressRepository::load(&mut tx, key, &progress.receipt)
                    .await
                    .expect("completed resources")
                    .len(),
                progress.fanout.len(),
                "all accepted resources must retain durable progress"
            );
        }
        assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
            .await
            .expect("complete receipts"));
        tx.commit().await.expect("inspection commit");
        for receiver in &mut receivers {
            assert_eq!(
                body(receiver.try_recv().expect("prompt room delivery")),
                text
            );
            assert!(
                receiver.try_recv().is_err(),
                "persistence retry must not resend"
            );
        }
    }
    fixture.close().await;
}

async fn reply_during_in_flight_reflection(release_predecessor: bool) {
    use crate::ingress::execute::test_hooks;

    let fixture = IngressFixture::sqlite().await;
    let state = shared_state(&fixture).await;
    let room: jid::BareJid = "in-flight-reflection@muc.example.com"
        .parse()
        .expect("room");
    let sender = fixture.submission(None, "").sender;
    let other: jid::FullJid = "juliet@example.com/phone".parse().expect("other occupant");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "in-flight-reflection".into(),
            channel_id: "in-flight-reflection".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    let mut receivers = Vec::new();
    for (resource, nick) in [(&sender, "sender"), (&other, "other")] {
        let (tx, receiver) = tokio::sync::mpsc::channel(4);
        socket_tests::register_test_connection(&state, resource, tx).await;
        receivers.push(receiver);
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
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    let mut decisions = Vec::new();
    for (author, text) in [(&sender, "A"), (&other, "B")] {
        let mut submission = groupchat_submission(&fixture, &room, author, text, text);
        let message = submission.plan.sanitized_message.clone();
        plan_broadcast(&mut submission, &room, &message, &deps).await;
        // Finish the other occupant's copy before pausing A's reflection receipt.
        submission.plan.plan.sort_by_key(|planned| matches!(&planned.effect,
            Effect::External(effect) if crate::ingress::recorded::single_target(effect) == Some(&sender)));
        decisions.push(
            commit_submission(&fixture.uow, &submission, 1)
                .await
                .expect("commit room message"),
        );
    }
    let older = &decisions[0];
    let newer = &decisions[1];
    assert!(!older.archive_ids.is_empty());
    assert!(!newer.archive_ids.is_empty());
    let append =
        test_hooks::pause_after_delivery_append(older.message_key.expect("A key"), sender.clone());
    let blocked = test_hooks::pause_after_blocked_dispatch(newer.message_key.expect("B key"));
    let completed = tokio::sync::Notify::new();
    let older_execution = async {
        let report = execute_effects(
            &fixture.uow,
            &fixture.db,
            older,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        completed.notify_one();
        report
    };
    let newer_execution = async {
        append.wait_until_reached().await;
        let execute = execute_effects(
            &fixture.uow,
            &fixture.db,
            newer,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        );
        let finish_predecessor = async {
            blocked.wait_until_reached().await;
            if release_predecessor {
                append.release();
                completed.notified().await;
            }
            blocked.release();
        };
        let (report, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(execute, finish_predecessor)
        })
        .await
        .expect("predecessor probes finish before the five-second execution deadline");
        if !release_predecessor {
            append.release();
        }
        report
    };
    let (older_report, newer_report) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(older_execution, newer_execution)
    })
    .await
    .expect("both live executions finish without maintenance");
    for report in [&older_report, &newer_report] {
        assert!(report.receipt_failures.is_empty(), "{report:?}");
        assert!(report.frame_obligations.is_empty(), "{report:?}");
    }
    assert!(older_report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    let reply_outcome = newer_report
        .outcomes
        .iter()
        .find(|(effect, _)| crate::ingress::recorded::single_target(effect) == Some(&sender))
        .map(|(_, outcome)| *outcome)
        .expect("reply to predecessor's sender");
    assert_eq!(
        reply_outcome,
        if release_predecessor {
            ExternalOutcome::Done
        } else {
            ExternalOutcome::AwaitingPredecessor
        },
        "only a completed predecessor releases its successor"
    );
    assert!(
        newer_report
            .outcomes
            .iter()
            .filter(|(effect, _)| crate::ingress::recorded::single_target(effect) != Some(&sender))
            .all(|(_, outcome)| *outcome == ExternalOutcome::Done),
        "ready sibling still completes"
    );
    for (index, receiver) in receivers.iter_mut().enumerate() {
        assert_eq!(body(receiver.try_recv().expect("older copy")), "A");
        if release_predecessor || index == 1 {
            assert_eq!(body(receiver.try_recv().expect("prompt reply")), "B");
        }
        assert!(receiver.try_recv().is_err(), "exactly one copy per message");
    }
    let mut tx = fixture.uow.begin().await.expect("inspect receipts");
    for (index, decision) in decisions.iter().enumerate() {
        assert_eq!(
            EffectReceiptRepository::receipts_complete(&mut tx, decision.message_key.expect("key"))
                .await
                .expect("complete receipts"),
            index == 0 || release_predecessor
        );
    }
    for progress in &newer.route_progress {
        let accepted = DeliveryProgressRepository::load(
            &mut tx,
            newer.message_key.expect("B key"),
            &progress.receipt,
        )
        .await
        .expect("B progress");
        for resource in &progress.fanout {
            assert_eq!(
                accepted.contains(resource),
                release_predecessor || resource != &sender,
                "a blocked copy cannot receive false delivery progress"
            );
        }
    }
    tx.commit().await.expect("inspection commit");
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_xep0045_reply_follows_in_flight_reflection_without_recovery_grace() {
    reply_during_in_flight_reflection(true).await;
}

#[tokio::test]
async fn sqlite_xep0045_persistent_predecessor_wait_is_bounded_and_preserves_ready_sibling() {
    reply_during_in_flight_reflection(false).await;
}
