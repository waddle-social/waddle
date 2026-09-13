use super::*;
use crate::ingress::execute_uow::FAIL_DELIVERY_PROGRESS_TX;
use std::sync::Arc;
use waddle_xmpp::stream_management::{
    DetachedSession, InMemorySmSessionRegistry, SmSessionRegistry,
};

async fn store_detached(sm: &InMemorySmSessionRegistry, resource: &jid::FullJid) {
    sm.store_session(DetachedSession {
        stream_id: resource.to_string(),
        user_id: resource.to_bare().to_string(),
        jid: resource.clone(),
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
    .expect("store detached session");
}

async fn append_count(sm: &InMemorySmSessionRegistry, resource: &jid::FullJid) -> usize {
    sm.peek_session(&resource.to_string())
        .await
        .expect("peek session")
        .expect("retained session")
        .unacked_stanzas
        .len()
}

#[derive(Clone, Copy)]
enum Case {
    Rollback,
    Concurrent,
    SenderOnly,
    Inbox,
}

async fn local_progress(fixture: IngressFixture, case: Case) {
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "local@muc.example.com".parse().expect("room");
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("occupant");
    let mut submission = fixture.submission(Some("local-progress"), "room content");
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
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    if !matches!(case, Case::SenderOnly) {
        store_detached(&sm, &target).await;
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
    if matches!(case, Case::SenderOnly) {
        // Isolate the empty MUC obligation: archival otherwise introduces an
        // outbound-activity sibling that legitimately awaits Phase C.
        waddle_xmpp::xep::xep0334::add_hint(&mut message, waddle_xmpp::xep::xep0334::Hint::NoStore);
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
    if matches!(case, Case::SenderOnly) {
        assert_eq!(
            submission.plan.intents,
            vec![muc.clone()],
            "no unrelated obligation delays terminality"
        );
    }
    let receipt = receipt_key(&muc).expect("receipt");
    let inbox = IngressEffectIntent::RouteDirect {
        recipient: target.to_bare(),
        fanout: vec![target.clone()],
        route_identity: waddle_xmpp::ingress::EffectMessageIdentity::capture_ordinal(99),
    };
    if matches!(case, Case::Inbox) {
        submission.plan.intents.push(inbox.clone());
    }
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    deps.effects = &ImmediateSink;
    if matches!(case, Case::SenderOnly) {
        let mut tx = fixture.uow.begin().await.expect("inspect commit");
        assert!(EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("empty MUC settled in commit"));
        assert!(
            crate::ingress_uow::CanonicalMessageRepository::is_terminal(&mut tx, key)
                .await
                .expect("terminal"),
            "sender-only aggregate settles inside commit before reflection executes"
        );
        tx.commit().await.expect("read commit");
        assert!(sender_rx.try_recv().is_err());
        assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    } else {
        let run = || {
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &decision,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            )
        };
        if matches!(case, Case::Rollback) {
            let report = FAIL_DELIVERY_PROGRESS_TX.scope(true, run()).await;
            assert!(
                report
                    .outcomes
                    .iter()
                    .any(|(_, o)| *o == ExternalOutcome::Uncertain),
                "{report:?}"
            );
            assert_eq!(append_count(&sm, &target).await, 1);
            assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
            assert!(!terminalize_if_complete(&fixture.uow, key)
                .await
                .expect("pending"));
            let retry = commit_submission(&fixture.uow, &submission, 1)
                .await
                .expect("retry");
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &retry,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            )
            .await;
        } else if matches!(case, Case::Concurrent) {
            let (a, b) = tokio::join!(run(), run());
            assert!(a.receipt_failures.is_empty(), "{a:?}");
            assert!(b.receipt_failures.is_empty(), "{b:?}");
        } else {
            run().await;
        }
        assert_eq!(
            append_count(&sm, &target).await,
            1,
            "one durable queue allocation"
        );
        assert_eq!(
            fixture.count("sm_ingress_appends").await,
            1,
            "append carries the MUC key"
        );
        let mut tx = fixture.uow.begin().await.expect("inspect");
        assert_eq!(
            DeliveryProgressRepository::load(&mut tx, key, &receipt)
                .await
                .expect("progress"),
            vec![target]
        );
        assert!(EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("MUC receipt"));
        if matches!(case, Case::Inbox) {
            let inbox_key = receipt_key(&inbox).expect("inbox receipt");
            assert!(!EffectReceiptRepository::contains(
                &mut tx,
                key,
                inbox_key.kind,
                &inbox_key.semantic_identity_hash
            )
            .await
            .expect("inbox pending"));
        }
        tx.commit().await.expect("read commit");
        assert_eq!(
            terminalize_if_complete(&fixture.uow, key)
                .await
                .expect("terminality"),
            !matches!(case, Case::Inbox)
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_muc_append_progress_rollback() {
    local_progress(IngressFixture::sqlite().await, Case::Rollback).await;
}
#[tokio::test]
async fn postgres_muc_append_progress_rollback() {
    if let Some(fixture) = IngressFixture::postgres("muc_append_progress_rollback").await {
        local_progress(fixture, Case::Rollback).await;
    }
}

#[tokio::test]
async fn sqlite_muc_concurrent_append() {
    local_progress(IngressFixture::sqlite().await, Case::Concurrent).await;
}
#[tokio::test]
async fn postgres_muc_concurrent_append() {
    if let Some(fixture) = IngressFixture::postgres("muc_concurrent_append").await {
        local_progress(fixture, Case::Concurrent).await;
    }
}

#[tokio::test]
async fn sqlite_muc_sender_only_commit() {
    local_progress(IngressFixture::sqlite().await, Case::SenderOnly).await;
}
#[tokio::test]
async fn postgres_muc_sender_only_commit() {
    if let Some(fixture) = IngressFixture::postgres("muc_sender_only_commit").await {
        local_progress(fixture, Case::SenderOnly).await;
    }
}

#[tokio::test]
async fn sqlite_muc_inbox_stays_pending() {
    local_progress(IngressFixture::sqlite().await, Case::Inbox).await;
}
#[tokio::test]
async fn postgres_muc_inbox_stays_pending() {
    if let Some(fixture) = IngressFixture::postgres("muc_inbox_stays_pending").await {
        local_progress(fixture, Case::Inbox).await;
    }
}
