use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::{
    interpret::{effects::PlanSink, interpret},
    websocket::{interpret_loop::build_interpret_deps, tests as socket_tests},
};
use std::sync::Arc;
use waddle_xmpp::{ingress::IngressEffectIntent, protocol::OutboundEvent};

async fn socket_state(
    fixture: &IngressFixture,
) -> Arc<crate::server::routes::websocket::WebSocketState> {
    let standalone = socket_tests::create_test_websocket_state().await;
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared database");
    socket_tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool),
        Arc::clone(&standalone.deps.protocol.ingress),
    )
    .await
}

async fn inbox_push_receipt(fixture: IngressFixture) {
    let state = socket_state(&fixture).await;
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("member");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &resource, tx).await;
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("sender");
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &sender, sender_tx).await;
    socket_tests::create_test_session(&state, "juliet").await;
    use waddle_xmpp::muc::{
        room_actor::{ChangeAffiliation, Join},
        room_registry_actor::CreateRoom,
    };
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "receipts".to_owned(),
            channel_id: "receipts".to_owned(),
            config: Default::default(),
        })
        .await
        .expect("create room");
    for (nick, occupant) in [("romeo", &sender), ("juliet", &resource)] {
        actor
            .ask(ChangeAffiliation {
                jid: occupant.to_bare(),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("member");
        actor
            .ask(Join {
                nick: nick.to_owned(),
                real_jid: occupant.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
    }
    let sink = PlanSink::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps =
        build_interpret_deps(&state, None).with_ingress_effect_capture(Some(capture.clone()));
    deps.effects = &sink;
    let mut submission = fixture.submission(None, "groupchat inbox");
    let mut message = submission.plan.sanitized_message.clone();
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.to = Some(room.clone().into());
    submission.target = waddle_xmpp::ingress::NormalizedTarget::Bare(room.clone());
    submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        &message,
        &waddle_xmpp::ingress::DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("groupchat digest");
    submission.plan.sanitized_message = message.clone();
    interpret(
        vec![OutboundEvent::DispatchToRoom {
            room,
            message: Box::new(message),
        }],
        &deps,
    )
    .await;
    let (plan, execution) = sink.take();
    submission.plan.plan = plan;
    submission.plan.room_execution = execution;
    submission.plan.intents = capture.snapshot().intents;
    assert_eq!(
        submission
            .plan
            .intents
            .iter()
            .filter(|intent| matches!(
                intent,
                IngressEffectIntent::RouteDirect {
                    route_identity: waddle_xmpp::ingress::EffectMessageIdentity::CaptureOrdinal(_),
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(rx.try_recv().is_err(), "planning sends nothing");
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit projection");
    let push_index = decision
        .external
        .iter()
        .position(|effect| {
            matches!(
                effect,
                ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
                    receipt: Some(_),
                    ..
                })
            )
        })
        .expect("planned push");
    assert_eq!(decision.external_receipts[push_index].len(), 1);
    deps.effects = &ImmediateSink;
    let mut report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    let mut push_received = false;
    while let Ok(envelope) = rx.try_recv() {
        if let Stanza::Message(message) = envelope.stanza {
            push_received |= message
                .payloads
                .iter()
                .any(|payload| payload.is("push", waddle_xmpp::xep::xep0430::NS_WADDLE_INBOX));
        }
    }
    assert!(push_received, "online member receives XEP-0430 push");
    assert!(sender_rx.try_recv().is_ok(), "sender receives reflection");
    report
        .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(5))
        .await
        .expect("frames delivered");
    let mut receipt_check = fixture.uow.begin().await.expect("receipt check");
    for intent in &submission.plan.intents {
        let key = crate::ingress::durable::receipt_key(intent).expect("intent identity");
        assert!(
            EffectReceiptRepository::contains(
                &mut receipt_check,
                decision.message_key.expect("canonical key"),
                key.kind,
                &key.semantic_identity_hash
            )
            .await
            .expect("receipt lookup"),
            "unreceipted intent: {intent:?}; outcomes: {:?}",
            report.outcomes
        );
    }
    receipt_check
        .commit()
        .await
        .expect("receipt check complete");
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        fixture.count("ingress_effect_intents").await
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close().await;
}

async fn offline_receipts(fixture: IngressFixture, fail_notification: bool) {
    use waddle_xmpp::pending_delivery::{
        storage::PendingDeliveryStorage, PendingPayload, QuotaPolicy,
    };
    let state = socket_state(&fixture).await;
    socket_tests::create_test_session(&state, "juliet").await;
    let pending: Arc<dyn PendingDeliveryStorage> = Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            Some(fixture.db.database_url()),
            QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending store"),
    );
    let sink = PlanSink::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps =
        build_interpret_deps(&state, None).with_ingress_effect_capture(Some(capture.clone()));
    deps.pending_delivery_storage = Some(&pending);
    deps.effects = &sink;
    let mut submission = fixture.submission(None, "offline receipt");
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let stamp =
        waddle_xmpp_core::xep0359::StanzaId::new("offline-archive", recipient.clone().into());
    use waddle_xmpp::mam::storage::MamStorage;
    let mam = waddle_xmpp::mam::SqlxMamStorage::open(fixture.db.database_url())
        .await
        .expect("archive store");
    let mut archived = waddle_xmpp::mam::ArchivedMessage::for_test(
        submission.sender.clone().into(),
        recipient.clone().into(),
    );
    archived.id = stamp.id.clone();
    archived.stanza_id = Some(stamp.clone());
    archived.body = Some("offline receipt".to_owned());
    mam.store_message(&recipient, &archived)
        .await
        .expect("committed recipient archive");
    interpret(
        vec![OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: PendingPayload::Archived(stamp),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(submission.plan.sanitized_message.clone()),
        }],
        &deps,
    )
    .await;
    submission.plan.plan = sink.take().0;
    submission.plan.intents = capture.snapshot().intents;
    assert_eq!(
        submission.plan.intents.len(),
        3,
        "pending, offline marker, notification candidate"
    );
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit offline intents");
    assert_eq!(decision.external_receipts[0].len(), 3);
    deps.effects = &ImmediateSink;
    if fail_notification {
        deps.web_socket_state = None;
    }
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert_eq!(
        pending.list(&recipient).await.expect("queued rows").len(),
        1
    );
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        if fail_notification { 1 } else { 3 }
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        if fail_notification { 0 } else { 1 }
    );
    assert_eq!(
        fixture.count("notification_candidates").await,
        if fail_notification { 0 } else { 1 }
    );
    assert_eq!(
        pending
            .list_unoutboxed_archived(10)
            .await
            .expect("outbox marker")
            .len(),
        usize::from(fail_notification)
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_groupchat_inbox_push_receipts_and_terminalizes() {
    inbox_push_receipt(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_groupchat_inbox_push_receipts_and_terminalizes() {
    if let Some(fixture) = IngressFixture::postgres("inbox_push_receipt").await {
        inbox_push_receipt(fixture).await;
    }
}
#[tokio::test]
async fn sqlite_offline_delivery_receipts_each_written_step() {
    offline_receipts(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn postgres_offline_delivery_receipts_each_written_step() {
    if let Some(fixture) = IngressFixture::postgres("offline_receipts").await {
        offline_receipts(fixture, false).await;
    }
}
#[tokio::test]
async fn sqlite_offline_delivery_partial_proof_leaves_notification_pending() {
    offline_receipts(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn postgres_offline_delivery_partial_proof_leaves_notification_pending() {
    if let Some(fixture) = IngressFixture::postgres("offline_partial_receipts").await {
        offline_receipts(fixture, true).await;
    }
}

#[test]
fn inbox_push_receipt_requires_every_frozen_resource() {
    let first: jid::FullJid = "juliet@example.com/phone".parse().expect("first");
    let second: jid::FullJid = "juliet@example.com/laptop".parse().expect("second");
    let intent = IngressEffectIntent::RouteDirect {
        recipient: first.to_bare(),
        fanout: vec![first.clone(), second.clone()],
        route_identity: waddle_xmpp::ingress::EffectMessageIdentity::CaptureOrdinal(5),
    };
    let key = crate::ingress::durable::receipt_key(&intent).expect("key");
    let effect = ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate {
        owner: first.to_bare(),
        projection: crate::ingress::effects::ProjectionRef(0),
        receipt: Some(Box::new(intent)),
    });
    assert!(proven_receipts(
        &effect,
        &EffectOutcome::InboxPush(vec![first.clone()]),
        std::slice::from_ref(&key)
    )
    .is_empty());
    assert_eq!(
        proven_receipts(
            &effect,
            &EffectOutcome::InboxPush(vec![first, second]),
            std::slice::from_ref(&key)
        ),
        vec![key]
    );
}
