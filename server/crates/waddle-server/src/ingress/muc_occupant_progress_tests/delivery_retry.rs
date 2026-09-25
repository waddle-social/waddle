//! Live delivery retries preserve ordering and avoid repeated socket writes.
use super::*;
use crate::server::routes::interpret::effects::Effect;
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

#[tokio::test]
async fn sqlite_xep0045_blocked_fanout_shares_probe_budget_and_reaches_ready_newcomer() {
    let fixture = IngressFixture::sqlite().await;
    let state = shared_state(&fixture).await;
    let room: jid::BareJid = "fanout-probe-budget@muc.example.com".parse().expect("room");
    let sender = fixture.submission(None, "").sender;
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "fanout-probe-budget".into(),
            channel_id: "fanout-probe-budget".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    let mut resources = vec![sender.clone()];
    resources.extend((1..8).map(|index| {
        format!("occupant{index}@example.com/phone")
            .parse::<jid::FullJid>()
            .expect("occupant")
    }));
    let newcomer: jid::FullJid = "newcomer@example.com/phone".parse().expect("newcomer");
    let mut receivers = Vec::new();
    for (index, resource) in resources
        .iter()
        .chain(std::iter::once(&newcomer))
        .enumerate()
    {
        let (tx, receiver) = tokio::sync::mpsc::channel(4);
        socket_tests::register_test_connection(&state, resource, tx).await;
        receivers.push(receiver);
        if resource != &newcomer {
            actor
                .ask(Join {
                    nick: format!("occupant{index}"),
                    real_jid: resource.clone(),
                    role: waddle_xmpp::Role::Participant,
                    affiliation: waddle_xmpp::Affiliation::Member,
                })
                .await
                .expect("old occupant joins");
        }
    }
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    let mut older = groupchat_submission(&fixture, &room, &sender, "budget-A", "A");
    let message = older.plan.sanitized_message.clone();
    plan_broadcast(&mut older, &room, &message, &deps).await;
    let older = commit_submission(&fixture.uow, &older, 1)
        .await
        .expect("commit pending predecessor");
    assert!(!older.archive_ids.is_empty());
    // This recipient has no obligation in A, so B must remain independently ready.
    actor
        .ask(Join {
            nick: "newcomer".into(),
            real_jid: newcomer.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("newcomer joins after A");
    let mut newer = groupchat_submission(&fixture, &room, &sender, "budget-B", "B");
    let message = newer.plan.sanitized_message.clone();
    plan_broadcast(&mut newer, &room, &message, &deps).await;
    newer.plan.plan.sort_by_key(|planned| matches!(&planned.effect,
        Effect::External(effect) if crate::ingress::recorded::single_target(effect) == Some(&newcomer)));
    let newer = commit_submission(&fixture.uow, &newer, 1)
        .await
        .expect("commit successor");
    let budget = crate::ingress::DispatchProbeBudget::default();
    deps.dispatch_probe_budget = Some(budget.clone());
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
    assert!(report.frame_obligations.is_empty(), "{report:?}");
    assert_eq!(
        budget.consumed_backoffs(),
        4,
        "all blocked recipients share four backoffs total"
    );
    for resource in &resources {
        let outcome = report
            .outcomes
            .iter()
            .find(|(effect, _)| crate::ingress::recorded::single_target(effect) == Some(resource))
            .map(|(_, outcome)| *outcome)
            .expect("old occupant effect");
        assert_eq!(outcome, ExternalOutcome::AwaitingPredecessor);
    }
    assert!(
        report
            .outcomes
            .iter()
            .any(
                |(effect, outcome)| crate::ingress::recorded::single_target(effect)
                    == Some(&newcomer)
                    && *outcome == ExternalOutcome::Done
            ),
        "ready newcomer completes after blocked recipients"
    );
    for receiver in &mut receivers[..resources.len()] {
        assert!(
            receiver.try_recv().is_err(),
            "blocked recipients receive no successor"
        );
    }
    let receiver = receivers.last_mut().expect("newcomer receiver");
    assert_eq!(
        body(receiver.try_recv().expect("newcomer's prompt copy")),
        "B"
    );
    assert!(receiver.try_recv().is_err(), "one newcomer copy");
    let mut tx = fixture.uow.begin().await.expect("inspect successor");
    let key = newer.message_key.expect("B key");
    for progress in &newer.route_progress {
        let accepted = DeliveryProgressRepository::load(&mut tx, key, &progress.receipt)
            .await
            .expect("delivery progress");
        for resource in &progress.fanout {
            assert_eq!(
                accepted.contains(resource),
                resource == &newcomer,
                "only the ready newcomer receives delivery progress"
            );
        }
    }
    assert!(!EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("pending receipt"));
    tx.commit().await.expect("inspection commit");
    fixture.close().await;
}

#[path = "delivery_rounds.rs"]
mod delivery_rounds;
