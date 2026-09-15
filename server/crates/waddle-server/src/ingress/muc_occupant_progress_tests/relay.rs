//! Controlled outcomes test ingress progress, not retry eligibility on a diverted relay channel.
use super::*;
use crate::ingress::execute_uow::FAIL_DELIVERY_PROGRESS_TX;
use crate::server::routes::interpret::{
    ControlledMucRelay, FullJidDeliveryOutcome, CONTROLLED_MUC_RELAY,
};
use std::sync::Arc;
use std::sync::Mutex;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

use super::local::{append_count, store_detached};

#[derive(Clone, Copy)]
enum Case {
    Partial,
    Declined,
    Uncertain,
    LocalBefore,
    LocalDuring,
}

async fn relay_progress(fixture: IngressFixture, case: Case) {
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "local@muc.example.com".parse().expect("room");
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("occupant");
    let mut submission = fixture.submission(Some("relay-progress"), "room content");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "local".into(),
            channel_id: "local".into(),
            config: Default::default(),
        })
        .await
        .expect("room");
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(16);
    socket_tests::register_test_connection(&state, &submission.sender, sender_tx).await;
    actor
        .ask(Join {
            nick: "romeo".into(),
            real_jid: submission.sender.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("sender join");
    let a: jid::FullJid = "alice@example.com/phone".parse().expect("A");
    let (a_tx, mut a_rx) = tokio::sync::mpsc::channel(16);
    socket_tests::register_test_connection(&state, &a, a_tx).await;
    actor
        .ask(Join {
            nick: "alice".into(),
            real_jid: a.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("A join");
    // The real planner needs a routable resource before selecting its copy.
    // Drop its receiver after planning; the controlled relay owns execution.
    let (target_tx, target_rx) = tokio::sync::mpsc::channel(16);
    socket_tests::register_test_connection(&state, &target, target_tx).await;
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    if matches!(case, Case::LocalBefore | Case::LocalDuring) {
        store_detached(&sm, &target).await;
    }
    {
        actor
            .ask(Join {
                nick: "juliet".into(),
                real_jid: target.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("occupant join");
    }
    let mut message = submission.plan.sanitized_message.clone();
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.to = Some(room.clone().into());
    waddle_xmpp::xep::xep0334::add_hint(&mut message, waddle_xmpp::xep::xep0334::Hint::NoStore);
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
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    deps.sm_session_registry = Some(&sm);
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    interpret(
        vec![OutboundEvent::DispatchToRoom {
            room,
            message: Box::new(message),
        }],
        &deps,
    )
    .await;
    submission.plan.room_canonical_message = sink.room_canonical_message();
    let (plan, execution) = sink.take();
    submission.plan.plan = plan;
    submission.plan.room_execution = execution;
    submission.plan.intents = capture.snapshot().intents;
    let muc = submission
        .plan
        .intents
        .iter()
        .find(|i| matches!(i, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("MUC intent")
        .clone();
    let receipt = receipt_key(&muc).expect("receipt");
    select_remote_copy(&mut submission, &target);
    drop(target_rx);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    deps.effects = &ImmediateSink;
    let targets = Arc::new(Mutex::new(Vec::new()));
    let controlled = match case {
        Case::Partial => ControlledMucRelay::Outcome(Some(FullJidDeliveryOutcome::Unavailable)),
        Case::Declined | Case::LocalBefore => ControlledMucRelay::Outcome(None),
        Case::Uncertain => {
            ControlledMucRelay::Outcome(Some(FullJidDeliveryOutcome::MaybeCommitted))
        }
        Case::LocalDuring => ControlledMucRelay::OwnerRefresh(sm.clone()),
    };
    let local = matches!(case, Case::LocalBefore | Case::LocalDuring);
    let run = CONTROLLED_MUC_RELAY.scope(
        (controlled.clone(), targets.clone()),
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        ),
    );
    let report = if local {
        FAIL_DELIVERY_PROGRESS_TX.scope(true, run).await
    } else {
        run.await
    };
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    if !local {
        let outcome = report
            .outcomes
            .iter()
            .find_map(|(effect, outcome)| match effect {
                ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid {
                    target: delivered,
                    ..
                }) if delivered == &target => Some(*outcome),
                _ => None,
            })
            .expect("relay outcome");
        assert_eq!(
            outcome,
            if matches!(case, Case::Uncertain) {
                ExternalOutcome::Uncertain
            } else {
                ExternalOutcome::Failed
            }
        );
    }

    assert_eq!(*targets.lock().expect("targets"), vec![target.clone()]);
    assert!(sender_rx.try_recv().is_ok(), "sender reflected");
    assert!(a_rx.try_recv().is_ok(), "A delivered");
    let mut tx = fixture.uow.begin().await.expect("initial progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("progress"),
        if local { vec![] } else { vec![a.clone()] }
    );
    tx.commit().await.expect("read commit");
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("pending"));
    if local {
        assert_eq!(append_count(&sm, &target).await, 1);
        let append_key = waddle_xmpp::stream_management::SmIngressAppendKey {
            message_key: key,
            kind: waddle_xmpp::stream_management::SmIngressReceiptKind::from_storage(
                receipt.kind.to_storage(),
            ),
            semantic_identity_hash: receipt.semantic_identity_hash,
            resource: target.clone(),
        };
        assert!(
            crate::sm_persistence::ingress_append::get(&fixture.db, &append_key)
                .await
                .expect("ledger")
                .is_some(),
            "owner transition uses the MUC append key"
        );
    }
    targets.lock().expect("targets").clear();
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("client retry");
    assert_eq!(retry.message_key, Some(key));
    let relay_targets: Vec<_> = retry
        .external
        .iter()
        .filter_map(|effect| match effect {
            ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { target, .. }) => {
                Some(target.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        relay_targets,
        vec![target.clone()],
        "retry retains pending remote copy"
    );
    let retry_outcome = if local {
        controlled
    } else {
        ControlledMucRelay::Outcome(Some(FullJidDeliveryOutcome::Delivered))
    };
    let report = CONTROLLED_MUC_RELAY
        .scope(
            (retry_outcome, targets.clone()),
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
    assert_eq!(*targets.lock().expect("targets"), vec![target.clone()]);
    let mut tx = fixture.uow.begin().await.expect("final progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("progress"),
        vec![a, target.clone()]
    );
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("aggregate"));
    tx.commit().await.expect("read commit");
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("terminal"));
    if local {
        assert_eq!(
            append_count(&sm, &target).await,
            1,
            "progress retry does not allocate again"
        );
        assert_eq!(fixture.count("sm_ingress_appends").await, 1);
    }
    if !local {
        assert!(a_rx.try_recv().is_err(), "completed A not repeated");
    }
    fixture.close().await;
}

fn select_remote_copy(submission: &mut IngressSubmission, target: &jid::FullJid) {
    let mut replaced = false;
    for planned in &mut submission.plan.plan {
        let crate::server::routes::interpret::effects::Effect::External(ExternalEffect::Delivery(
            delivery,
        )) = &mut planned.effect
        else {
            continue;
        };
        let stanza = match delivery {
            ExternalDeliveryEffect::RouteToPeer { jid, stanza, .. } if jid == target => {
                stanza.clone()
            }
            ExternalDeliveryEffect::QueueDetached {
                resources, stanza, ..
            } if resources == &vec![target.clone()] => stanza.clone(),
            _ => continue,
        };
        *delivery = ExternalDeliveryEffect::RelayFullJid {
            route_identity: None,
            origin: None,
            target: target.clone(),
            stanza,
            call_setup: None,
        };
        replaced = true;
    }
    assert!(replaced, "real planner emitted occupant copy");
}

macro_rules! cases {
    ($(($sqlite:ident, $postgres:ident, $case:ident)),+ $(,)?) => { $(
        #[tokio::test]
        async fn $sqlite() { relay_progress(IngressFixture::sqlite().await, Case::$case).await; }
        #[tokio::test]
        async fn $postgres() { if let Some(fixture) = IngressFixture::postgres(stringify!($case)).await { relay_progress(fixture, Case::$case).await; } }
    )+ };
}
cases!(
    (
        sqlite_muc_relay_partial_client_retry,
        postgres_muc_relay_partial_client_retry,
        Partial
    ),
    (
        sqlite_muc_relay_declined_pending,
        postgres_muc_relay_declined_pending,
        Declined
    ),
    (
        sqlite_muc_relay_uncertain_pending,
        postgres_muc_relay_uncertain_pending,
        Uncertain
    ),
    (
        sqlite_muc_relay_local_before_progress_rollback,
        postgres_muc_relay_local_before_progress_rollback,
        LocalBefore
    ),
    (
        sqlite_muc_relay_local_during_progress_rollback,
        postgres_muc_relay_local_during_progress_rollback,
        LocalDuring
    ),
);
