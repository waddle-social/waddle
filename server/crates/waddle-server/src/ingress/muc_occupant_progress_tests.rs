//! Real room planning, canonical commit and delivery execution regressions.
use super::{
    commit::commit_submission,
    effects::{delivery::ExternalDeliveryEffect, PlanSink},
    execute::{execute_effects, terminalize_if_complete},
    execute_uow::STALL_DELIVERY_RESOURCE,
    test_support::IngressFixture,
    *,
};
use crate::server::routes::interpret::DeliveryExecutionContext;
use crate::{
    ingress_uow::{DeliveryProgressRepository, EffectReceiptRepository},
    server::routes::{
        interpret::interpret,
        websocket::{interpret_loop::build_interpret_deps, tests as socket_tests},
    },
};
use waddle_xmpp::{
    ingress::{DigestContext, DigestInput, IngressEffectIntent, NormalizedTarget},
    muc::{room_actor::Join, room_registry_actor::CreateRoom},
    protocol::OutboundEvent,
};

async fn plan_broadcast(
    submission: &mut IngressSubmission,
    room: &jid::BareJid,
    message: &xmpp_parsers::message::Message,
    deps: &crate::server::routes::interpret::Deps<'_>,
) {
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let mut planned = deps.clone();
    planned.effects = &sink;
    planned.ingress_effect_capture = Some(capture.clone());
    interpret(
        vec![OutboundEvent::DispatchToRoom {
            room: room.clone(),
            message: Box::new(message.clone()),
        }],
        &planned,
    )
    .await;
    submission.plan.room_canonical_message = sink.room_canonical_message();
    let (plan, execution) = sink.take();
    submission.plan.plan = plan;
    submission.plan.room_execution = execution;
    submission.plan.intents = capture.snapshot().intents;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetryCase {
    Ordinary,
    ReconnectedSender,
    MissingProvenance,
}

async fn partial_broadcast(fixture: IngressFixture, bodyless: bool, retry_case: RetryCase) {
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let a: jid::FullJid = "alice@example.com/phone".parse().expect("A");
    let b: jid::FullJid = "ben@example.com/phone".parse().expect("B");
    let mut submission = fixture.submission(Some("muc-progress"), "original room content");
    let sender = submission.sender.clone();
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "progress".into(),
            channel_id: "progress".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    let mut receivers = Vec::new();
    for (resource, nick) in [(&a, "alice"), (&b, "ben"), (&sender, "romeo")] {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        socket_tests::register_test_connection(&state, resource, tx).await;
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
    if bodyless {
        message.bodies.clear();
        message
            .payloads
            .push(waddle_xmpp::xep::xep0085::build_chat_state_element(
                waddle_xmpp::xep::xep0085::ChatState::Composing,
            ));
    }
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
    let deps = {
        let mut deps = build_interpret_deps(&state, None);
        deps.inbox_storage = None;
        deps
    };
    plan_broadcast(&mut submission, &room, &message, &deps).await;
    if bodyless {
        assert!(
            !submission.plan.intents.iter().any(|intent| matches!(
                intent,
                IngressEffectIntent::ArchiveAuthoritative { .. }
                    | IngressEffectIntent::SystemMessageArchive { .. }
            )),
            "chat-state broadcasts have no archive stamp to restore from"
        );
    }
    // Preserve real effects and policies, executing B last so the timeout also
    // proves sender reflection and A's progress happened beforehand.
    submission.plan.plan.sort_by_key(|planned| matches!(&planned.effect,
        crate::server::routes::interpret::effects::Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. })) if jid == &b));
    let intent = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("real room fanout");
    let receipt = receipt_key(intent).expect("kind-2 receipt");
    assert!(receivers.iter_mut().all(|rx| rx.try_recv().is_err()));
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    let stalled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let report = STALL_DELIVERY_RESOURCE
        .scope(
            (b.clone(), stalled.clone()),
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &decision,
                &ImmediateSink,
                &deps,
                Duration::from_secs(1),
            ),
        )
        .await;
    assert!(
        stalled.load(std::sync::atomic::Ordering::SeqCst),
        "B entered the progress arm"
    );
    assert!(
        report
            .outcomes
            .iter()
            .any(|(_, outcome)| *outcome == ExternalOutcome::Uncertain),
        "timeout leaves B pending"
    );
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(receivers[0].try_recv().is_ok(), "A delivered");
    assert!(receivers[1].try_recv().is_err(), "B failed before delivery");
    assert!(receivers[2].try_recv().is_ok(), "sender reflected");
    let mut tx = fixture.uow.begin().await.expect("inspect");
    let progress = DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("progress");
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("aggregate"));
    tx.commit().await.expect("read commit");
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("pending")
    );
    let c: jid::FullJid = "claire@example.com/phone".parse().expect("C");
    let (tx, mut c_rx) = tokio::sync::mpsc::channel(16);
    socket_tests::register_test_connection(&state, &c, tx).await;
    actor
        .ask(Join {
            nick: "claire".into(),
            real_jid: c.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("late join");
    if retry_case == RetryCase::MissingProvenance {
        let mut tx = fixture.uow.begin().await.expect("old row");
        crate::ingress_uow::CanonicalMessageRepository::record_room_canonical_envelope(
            &mut tx,
            key,
            &crate::ingress_substrate::MessageEnvelope::new(message.clone()),
        )
        .await
        .expect("pre-fix real-sender envelope");
        tx.commit().await.expect("old row commit");
    }
    let mut retry_sender = sender.clone();
    if retry_case == RetryCase::ReconnectedSender {
        use waddle_xmpp::muc::room_actor::{
            LeaveAttemptId, LeaveByRealJid, LeaveOrigin, LeaveSessionSelector,
        };
        actor
            .ask(LeaveByRealJid {
                sender_jid: sender.clone(),
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: LeaveOrigin::Fresh,
            })
            .await
            .expect("sender leaves old resource");
        retry_sender = sender
            .to_bare()
            .with_resource_str("new")
            .expect("new resource");
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        socket_tests::register_test_connection(&state, &retry_sender, tx).await;
        receivers[2] = rx;
        actor
            .ask(Join {
                nick: "romeo".into(),
                real_jid: retry_sender.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("sender rejoins");
        submission.sender = retry_sender.clone();
        message.from = Some(retry_sender.clone().into());
        submission.plan.sanitized_message = message.clone();
    }
    // Replanning creates a new room stanza ID and includes the new occupant;
    // replay must recover the original stamp and frozen audience.
    plan_broadcast(&mut submission, &room, &message, &deps).await;
    if retry_case == RetryCase::ReconnectedSender {
        // The full ingress planner returns this rewritten prototype; reflection
        // detection must also honor its sender-copy policy marker.
        submission.plan.sanitized_message = submission
            .plan
            .room_canonical_message
            .as_deref()
            .expect("room prototype")
            .clone();
    }
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("duplicate");
    let targets: Vec<_> = retry
        .external
        .iter()
        .filter_map(|effect| match effect {
            ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. }) => {
                Some(jid.clone())
            }
            _ => None,
        })
        .collect();
    eprintln!("P1 premise: durable progress={progress:?}; duplicate live targets={targets:?}");
    assert_eq!(
        progress,
        vec![a.clone()],
        "P1: A must have durable kind-2 progress before retry"
    );
    assert_eq!(
        targets.contains(&b),
        retry_case != RetryCase::MissingProvenance,
        "only non-sender copies require frozen provenance"
    );
    assert!(
        targets.contains(&retry_sender),
        "current sender reflection remains Always"
    );
    assert!(!targets.contains(&a), "completed A is not repeated");
    assert!(
        !targets.contains(&c),
        "late occupant C does not widen frozen audience"
    );
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert_eq!(
        receivers[1].try_recv().is_ok(),
        retry_case != RetryCase::MissingProvenance
    );
    assert!(receivers[0].try_recv().is_err(), "A receives no duplicate");
    assert!(
        receivers[2].try_recv().is_ok(),
        "S receives duplicate reflection"
    );
    assert!(c_rx.try_recv().is_err(), "C receives no historical copy");
    if retry_case == RetryCase::MissingProvenance {
        assert!(
            !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
                .await
                .expect("old row pending")
        );
        let mut tx = fixture.uow.begin().await.expect("old source");
        let envelope = crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("load")
            .expect("envelope");
        let intent = retry.route_progress[0].settle_evidence();
        assert_eq!(
            super::room_canonical::source(&envelope, &intent),
            Err(super::room_canonical::CanonicalSourceError::MissingCanonicalProvenance)
        );
        assert_eq!(
            DeliveryProgressRepository::load(&mut tx, key, &receipt)
                .await
                .expect("progress"),
            vec![a]
        );
        tx.commit().await.expect("read");
        fixture.close().await;
        return;
    }
    let mut tx = fixture.uow.begin().await.expect("final progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("progress"),
        vec![a, b]
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
    assert!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("complete")
    );
    fixture.close().await;
}
#[tokio::test]
async fn sqlite_muc_occupant_progress_partial_broadcast() {
    partial_broadcast(IngressFixture::sqlite().await, false, RetryCase::Ordinary).await;
}
#[tokio::test]
async fn postgres_muc_occupant_progress_partial_broadcast() {
    if let Some(fixture) = IngressFixture::postgres("muc_occupant_progress").await {
        partial_broadcast(fixture, false, RetryCase::Ordinary).await;
    }
}

#[tokio::test]
async fn sqlite_muc_occupant_progress_archive_free_replan() {
    partial_broadcast(IngressFixture::sqlite().await, true, RetryCase::Ordinary).await;
}
#[tokio::test]
async fn postgres_muc_occupant_progress_archive_free_replan() {
    if let Some(fixture) = IngressFixture::postgres("muc_archive_free_replan").await {
        partial_broadcast(fixture, true, RetryCase::Ordinary).await;
    }
}

#[path = "muc_occupant_progress_tests/canonical.rs"]
mod canonical;

#[path = "muc_occupant_progress_tests/local.rs"]
mod local;

#[path = "muc_occupant_progress_tests/system.rs"]
mod system;

#[path = "muc_occupant_progress_tests/replay.rs"]
mod replay;

#[cfg(feature = "clustering")]
#[path = "muc_occupant_progress_tests/relay.rs"]
mod relay;

#[cfg(feature = "clustering")]
#[path = "muc_occupant_progress_tests/stalled_relay.rs"]
mod stalled_relay;

#[tokio::test]
async fn sqlite_muc_occupant_progress_reconnected_sender() {
    partial_broadcast(
        IngressFixture::sqlite().await,
        false,
        RetryCase::ReconnectedSender,
    )
    .await;
}
#[tokio::test]
async fn postgres_muc_occupant_progress_reconnected_sender() {
    if let Some(fixture) = IngressFixture::postgres("muc_reconnected_sender").await {
        partial_broadcast(fixture, false, RetryCase::ReconnectedSender).await;
    }
}
#[tokio::test]
async fn sqlite_muc_occupant_progress_old_provenance_reflection() {
    partial_broadcast(
        IngressFixture::sqlite().await,
        false,
        RetryCase::MissingProvenance,
    )
    .await;
}
#[tokio::test]
async fn postgres_muc_occupant_progress_old_provenance_reflection() {
    if let Some(fixture) = IngressFixture::postgres("muc_old_provenance_reflection").await {
        partial_broadcast(fixture, false, RetryCase::MissingProvenance).await;
    }
}

#[path = "muc_occupant_progress_tests/sibling_retry.rs"]
mod sibling_retry;

#[path = "muc_occupant_progress_tests/host_owned.rs"]
mod host_owned;

#[cfg(feature = "clustering")]
#[path = "muc_occupant_progress_tests/relayed_sibling.rs"]
mod relayed_sibling;
