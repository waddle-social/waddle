//! Shared retry rounds must give later recipients a chance to unblock.
use super::*;

#[tokio::test]
async fn sqlite_xep0045_later_blocked_recipient_gets_fair_rechecks_without_repeating_effects() {
    use crate::ingress::execute::test_hooks;
    use std::sync::Arc;

    let fixture = IngressFixture::sqlite().await;
    let mut state = shared_state(&fixture).await;
    Arc::get_mut(&mut state)
        .expect("unique state")
        .deps
        .protocol
        .extension_manager =
        crate::server::routes::interpret::tests::room_dispatch::room_observer_test_manager(
            crate::server::routes::interpret::tests::room_dispatch::ObserverConfiguration::Observer,
        )
        .await;
    let room: jid::BareJid = "fair-recheck@muc.example.com".parse().expect("room");
    let sender = fixture.submission(None, "").sender;
    let persistent: jid::FullJid = "persistent@example.com/phone".parse().expect("persistent");
    let later: jid::FullJid = "later@example.com/phone".parse().expect("later");
    let ready: jid::FullJid = "later@example.com/laptop".parse().expect("ready");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "fair-recheck".into(),
            channel_id: "fair-recheck".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    let mut receivers = Vec::new();
    for resource in [&sender, &persistent, &later, &ready] {
        let (tx, receiver) = tokio::sync::mpsc::channel(4);
        socket_tests::register_test_connection(&state, resource, tx).await;
        receivers.push(receiver);
    }
    for (resource, nick) in [(&sender, "sender"), (&persistent, "persistent")] {
        actor
            .ask(Join {
                nick: nick.into(),
                real_jid: resource.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("initial occupant");
    }
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    let mut decisions = Vec::new();
    for (text, newcomer) in [("A", None), ("B", Some(&later)), ("C", Some(&ready))] {
        if let Some(resource) = newcomer {
            actor
                .ask(Join {
                    nick: text.into(),
                    real_jid: resource.clone(),
                    role: waddle_xmpp::Role::Participant,
                    affiliation: waddle_xmpp::Affiliation::Member,
                })
                .await
                .expect("new occupant joins after predecessor audience was frozen");
        }
        let mut submission = groupchat_submission(&fixture, &room, &sender, text, text);
        let message = submission.plan.sanitized_message.clone();
        plan_broadcast(&mut submission, &room, &message, &deps).await;
        if text == "C" {
            assert_eq!(
                submission
                    .plan
                    .intents
                    .iter()
                    .filter(|intent| matches!(intent, IngressEffectIntent::RoomObserver { .. }))
                    .count(),
                1,
                "configured room observation must freeze one successor intent"
            );
        }
        // B pauses on later's accepted socket append. C must probe the persistent
        // target before later, after already completing an independent target.
        submission.plan.plan.sort_by_key(|planned| {
            let Effect::External(effect) = &planned.effect else {
                return 0;
            };
            let target = crate::ingress::recorded::single_target(effect);
            if text == "B" {
                usize::from(target != Some(&later))
            } else if target == Some(&ready) {
                0
            } else if target == Some(&persistent) {
                1
            } else if target == Some(&later) {
                2
            } else {
                3
            }
        });
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("commit frozen room audience");
        assert!(
            !decision.archive_ids.is_empty(),
            "exercise archive ordering"
        );
        decisions.push(decision);
    }
    // A is deliberately never executed, keeping the first blocked C recipient
    // pending. Later joined after A, so its distinct predecessor is B alone.
    let predecessor = &decisions[1];
    let successor = &decisions[2];
    assert_eq!(
        successor
            .external
            .iter()
            .filter(|effect| matches!(
                effect,
                ExternalEffect::Room(effects::room::ExternalRoomEffect::ObserveRoomMessage { .. })
            ))
            .count(),
        1,
        "the committed successor retains one frozen observer effect"
    );
    let append = test_hooks::pause_after_delivery_append(
        predecessor.message_key.expect("B key"),
        later.clone(),
    );
    let blocked = test_hooks::pause_after_blocked_dispatch_for_resource(
        successor.message_key.expect("C key"),
        later.clone(),
    );
    let predecessor_completed = tokio::sync::Notify::new();
    let budget = crate::ingress::DispatchProbeBudget::default();
    let mut successor_deps = deps.clone();
    successor_deps.dispatch_probe_budget = Some(budget.clone());
    let predecessor_execution = async {
        let report = execute_effects(
            &fixture.uow,
            &fixture.db,
            predecessor,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        predecessor_completed.notify_one();
        report
    };
    let successor_execution = async {
        append.wait_until_reached().await;
        let execute = execute_effects(
            &fixture.uow,
            &fixture.db,
            successor,
            &ImmediateSink,
            &successor_deps,
            Duration::from_secs(5),
        );
        let finish_predecessor = async {
            blocked.wait_until_reached().await;
            append.release();
            predecessor_completed.notified().await;
            blocked.release();
        };
        let (report, ()) = tokio::join!(execute, finish_predecessor);
        report
    };
    let (predecessor_report, report) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(predecessor_execution, successor_execution)
    })
    .await
    .expect("both live executions finish using deterministic dispatch gates");
    for result in [&predecessor_report, &report] {
        assert!(result.receipt_failures.is_empty(), "{result:?}");
        assert!(result.frame_obligations.is_empty(), "{result:?}");
    }
    for resource in [&sender, &persistent, &later, &ready] {
        let outcome = report
            .outcomes
            .iter()
            .find(|(effect, _)| crate::ingress::recorded::single_target(effect) == Some(resource))
            .map(|(_, outcome)| *outcome)
            .expect("successor recipient");
        assert_eq!(
            outcome,
            if resource == &later || resource == &ready {
                ExternalOutcome::Done
            } else {
                ExternalOutcome::AwaitingPredecessor
            },
            "later receives a recheck even while an earlier recipient stays blocked: {resource}"
        );
    }
    assert_eq!(budget.consumed_backoffs(), 4, "one shared bounded budget");
    assert!(receivers[0].try_recv().is_err(), "sender remains blocked");
    assert!(
        receivers[1].try_recv().is_err(),
        "persistent remains blocked"
    );
    assert_eq!(
        body(receivers[2].try_recv().expect("later predecessor")),
        "B"
    );
    assert_eq!(body(receivers[2].try_recv().expect("later successor")), "C");
    assert_eq!(body(receivers[3].try_recv().expect("ready successor")), "C");
    assert!(
        receivers
            .iter_mut()
            .all(|receiver| receiver.try_recv().is_err()),
        "rechecks never repeat accepted socket writes"
    );
    assert_eq!(
        report
            .outcomes
            .iter()
            .filter(|(effect, outcome)| {
                matches!(
                    effect,
                    ExternalEffect::Room(
                        effects::room::ExternalRoomEffect::ObserveRoomMessage { .. }
                    )
                ) && *outcome == ExternalOutcome::AwaitingPredecessor
            })
            .count(),
        1,
        "rechecks schedule the frozen observer obligation once"
    );
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("inspect successor progress");
    let key = successor.message_key.expect("C key");
    for progress in &successor.route_progress {
        let accepted = DeliveryProgressRepository::load(&mut tx, key, &progress.receipt)
            .await
            .expect("successor accepted resources");
        for resource in &progress.fanout {
            assert_eq!(
                accepted.contains(resource),
                resource == &later || resource == &ready,
                "only completed recipients receive durable progress"
            );
        }
    }
    assert!(!EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("persistent recipient keeps aggregate receipt pending"));
    tx.commit().await.expect("inspection commit");
    fixture.close().await;
}
