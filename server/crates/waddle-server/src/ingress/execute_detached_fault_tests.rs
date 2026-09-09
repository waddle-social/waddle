//! Durable progress survives executor cancellation and rolls back on storage faults.
use super::*;
use crate::{
    ingress::{
        commit::commit_submission,
        execute_uow::{FAIL_DELIVERY_PROGRESS_TX, STALL_DELIVERY_RESOURCE},
        test_support::IngressFixture,
    },
    ingress_uow::DeliveryProgressRepository,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent},
    stream_management::{DetachedSession, InMemorySmSessionRegistry, SmSessionRegistry},
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

async fn detached_progress_fault(fixture: IngressFixture, rollback: bool) {
    detached_delivery_fault(fixture, rollback, None, false).await;
}

async fn detached_delivery_fault(
    fixture: IngressFixture,
    rollback: bool,
    live_kind: Option<crate::server::routes::interpret::effects::delivery::PeerDeliveryKind>,
    single: bool,
) {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let first: jid::FullJid = "juliet@example.com/phone".parse().expect("first");
    let second: jid::FullJid = "juliet@example.com/laptop".parse().expect("second");
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let mut live_receivers = Vec::new();
    for resource in [&first, &second] {
        if live_kind.is_some() {
            let (sender, receiver) = tokio::sync::mpsc::channel(4);
            state
                .deps
                .protocol
                .connection_registry
                .register_with_carbons(resource.clone(), sender, false);
            assert!(
                crate::server::dual_registration::mirror_register(
                    &state.deps.protocol.user_registry,
                    resource.clone(),
                    state
                        .deps
                        .protocol
                        .connection_registry
                        .get_entry(resource)
                        .expect("live entry"),
                )
                .await
            );
            live_receivers.push(receiver);
        } else {
            store_detached(&sm, resource).await;
        }
    }
    let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.sm_session_registry = Some(&sm);
    let mut submission = fixture.submission(Some("detached-fault"), "canonical delivery");
    let identity = EffectMessageIdentity::capture_ordinal(1);
    let intent = IngressEffectIntent::RouteDirect {
        recipient: first.to_bare(),
        fanout: if single {
            vec![first.clone()]
        } else {
            vec![first.clone(), second.clone()]
        },
        route_identity: identity.clone(),
    };
    let receipt = crate::ingress::receipt_key(&intent).expect("receipt");
    submission.plan.intents = vec![intent];
    let sink = crate::server::routes::interpret::effects::PlanSink::new();
    let mut planned = deps.clone();
    planned.effects = &sink;
    crate::server::routes::interpret::effects::delivery::record(
        &planned,
        ExternalDeliveryEffect::QueueDetached {
            route_identity: Some(identity),
            call_setup: None,
            bare: first.to_bare(),
            resources: vec![first.clone(), second.clone()],
            stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
        },
    );
    submission.plan.plan = sink.take().0;
    if let Some(kind) = live_kind {
        use crate::server::routes::interpret::{
            deliver_direct_to_full_with_registered_remote,
            deliver_peer_to_full_with_registered_remote, effects::delivery::PeerDeliveryKind,
        };
        planned.direct_route_identity = Some(EffectMessageIdentity::capture_ordinal(1));
        for resource in [&first, &second]
            .into_iter()
            .take(if single { 1 } else { 2 })
        {
            assert!(state
                .deps
                .protocol
                .connection_registry
                .get_entry(resource)
                .is_some());
            let stanza = Stanza::Message(submission.plan.sanitized_message.clone());
            match kind {
                PeerDeliveryKind::PeerStanza => {
                    deliver_peer_to_full_with_registered_remote(&planned, resource, &stanza).await;
                }
                PeerDeliveryKind::DirectFrame => {
                    deliver_direct_to_full_with_registered_remote(&planned, resource, &stanza)
                        .await;
                }
                PeerDeliveryKind::RegistryFrame => panic!("test covers actor delivery variants"),
            }
        }
        submission.plan.plan = sink.take().0;
    }
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("canonical key");
    if live_kind.is_some() {
        // Disconnect only after the live routes and exact receipt are frozen.
        for resource in [&first, &second] {
            state.deps.protocol.connection_registry.unregister(resource);
            state
                .deps
                .protocol
                .user_registry
                .ask(waddle_xmpp::registry::UnregisterUserResource {
                    jid: resource.clone(),
                    owner: None,
                })
                .await
                .expect("unregister live resource");
            store_detached(&sm, resource).await;
        }
        assert!(live_receivers
            .iter_mut()
            .all(|receiver| receiver.try_recv().is_err()));
    }
    let execute = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(1),
    );
    let report = if rollback {
        FAIL_DELIVERY_PROGRESS_TX.scope(true, execute).await
    } else {
        let entered = Arc::new(AtomicBool::new(false));
        let report = STALL_DELIVERY_RESOURCE
            .scope((second.clone(), entered.clone()), execute)
            .await;
        assert!(
            entered.load(Ordering::SeqCst),
            "B stalls only after A's progress commits"
        );
        report
    };
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
    let mut tx = fixture.uow.begin().await.expect("inspect progress");
    let progress = DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("progress");
    assert_eq!(
        progress,
        if rollback {
            Vec::new()
        } else {
            vec![first.clone()]
        }
    );
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("receipt"));
    tx.commit().await.expect("read commit");
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("not terminal"));
    assert_eq!(append_count(&sm, &first).await, 1);
    assert_eq!(
        append_count(&sm, &second).await,
        usize::from(live_kind.is_some() && !single)
    );

    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("ordinary duplicate");
    assert_eq!(replay.message_key, Some(key));
    assert_eq!(replay.route_progress[0].completed, progress);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(report.receipt_failures.is_empty());
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("terminal"));
    assert_eq!(
        append_count(&sm, &first).await,
        1,
        "the durable append ledger prevents repetition even when progress rolled back"
    );
    assert_eq!(append_count(&sm, &second).await, usize::from(!single));
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_detached_progress_rollback_retries_only_unrecorded_appends() {
    detached_progress_fault(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn postgres_detached_progress_rollback_retries_only_unrecorded_appends() {
    if let Some(fixture) = IngressFixture::postgres("detached_progress_rollback").await {
        detached_progress_fault(fixture, true).await;
    }
}
#[tokio::test]
async fn sqlite_detached_progress_timeout_preserves_completed_resource() {
    detached_progress_fault(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn postgres_detached_progress_timeout_preserves_completed_resource() {
    if let Some(fixture) = IngressFixture::postgres("detached_progress_timeout").await {
        detached_progress_fault(fixture, false).await;
    }
}

async fn detached_muc_keeps_generic_settlement(fixture: IngressFixture) {
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
    deps.sm_session_registry = Some(&sm);
    let mut submission = fixture.submission(Some("detached-muc"), "room message");
    let target = submission.sender.clone();
    store_detached(&sm, &target).await;
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let identity = EffectMessageIdentity::capture_ordinal(1);
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
    message.from = Some(room.with_resource_str("romeo").expect("room nick").into());
    message.to = Some(target.clone().into());
    submission.plan.intents = vec![IngressEffectIntent::RouteMucGroupchat {
        room,
        occupants: vec![target.clone()],
        reflection: target.clone(),
        room_generation: waddle_xmpp::ingress::EntityGeneration::INITIAL,
        route_identity: identity.clone(),
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            route_identity: Some(identity),
            call_setup: None,
            bare: target.to_bare(),
            resources: vec![target.clone()],
            stanza: Box::new(Stanza::Message(message)),
        }),
    ))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("MUC commit");
    assert!(decision.route_progress.is_empty());
    assert!(
        decision.arm_owned_receipts.is_empty(),
        "MUC remains a generic delivery obligation"
    );
    assert_eq!(decision.external_receipts[0].len(), 1);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(append_count(&sm, &target).await, 1);
    assert!(
        terminalize_if_complete(&fixture.uow, decision.message_key.expect("key"))
            .await
            .expect("MUC aggregate settled")
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_detached_muc_without_direct_progress_keeps_generic_receipt() {
    detached_muc_keeps_generic_settlement(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_detached_muc_without_direct_progress_keeps_generic_receipt() {
    if let Some(fixture) = IngressFixture::postgres("detached_muc_generic").await {
        detached_muc_keeps_generic_settlement(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_planned_live_detached_progress_rollback() {
    use crate::server::routes::interpret::effects::delivery::PeerDeliveryKind;
    for kind in [PeerDeliveryKind::PeerStanza, PeerDeliveryKind::DirectFrame] {
        for single in [true, false] {
            detached_delivery_fault(IngressFixture::sqlite().await, true, Some(kind), single).await;
        }
    }
}

#[tokio::test]
async fn postgres_planned_live_detached_progress_rollback() {
    use crate::server::routes::interpret::effects::delivery::PeerDeliveryKind;
    for kind in [PeerDeliveryKind::PeerStanza, PeerDeliveryKind::DirectFrame] {
        for single in [true, false] {
            if let Some(fixture) = IngressFixture::postgres("planned_live_detached_rollback").await
            {
                detached_delivery_fault(fixture, true, Some(kind), single).await;
            }
        }
    }
}

async fn mixed_direct_muc_context_isolation(fixture: IngressFixture) {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
    deps.sm_session_registry = Some(&sm);
    let mut submission = fixture.submission(Some("mixed-detached"), "mixed delivery");
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    store_detached(&sm, &target).await;
    submission.plan.intents.clear();
    submission.plan.plan.clear();
    // Direct obligations surround the generic MUC delivery and share its
    // resource. A leaked direct context would suppress or ledger the MUC copy.
    for ordinal in 1..=3 {
        let identity = EffectMessageIdentity::capture_ordinal(ordinal);
        let mut message = submission.plan.sanitized_message.clone();
        let intent = if ordinal == 2 {
            message.type_ = xmpp_parsers::message::MessageType::Groupchat;
            message.from = Some(room.with_resource_str("romeo").expect("room nick").into());
            message.to = Some(target.clone().into());
            IngressEffectIntent::RouteMucGroupchat {
                room: room.clone(),
                occupants: vec![target.clone()],
                reflection: target.clone(),
                room_generation: waddle_xmpp::ingress::EntityGeneration::INITIAL,
                route_identity: identity.clone(),
            }
        } else {
            IngressEffectIntent::RouteDirect {
                recipient: target.to_bare(),
                fanout: vec![target.clone()],
                route_identity: identity.clone(),
            }
        };
        submission.plan.intents.push(intent);
        submission
            .plan
            .plan
            .push(PlannedEffect::new(Effect::External(
                ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
                    route_identity: Some(identity),
                    call_setup: None,
                    bare: target.to_bare(),
                    resources: vec![target.clone()],
                    stanza: Box::new(Stanza::Message(message)),
                }),
            )));
    }
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("mixed commit");
    assert_eq!(decision.route_progress.len(), 2);
    assert_eq!(decision.arm_owned_receipts.len(), 2);
    assert_eq!(decision.external_receipts.len(), 3);
    let execute = || {
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
    };
    // Both direct appends survive failed progress transactions. MUC settles
    // generically and must not inherit either direct append identity.
    let report = FAIL_DELIVERY_PROGRESS_TX.scope(true, execute()).await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert_eq!(append_count(&sm, &target).await, 3);
    assert_eq!(fixture.count("sm_ingress_appends").await, 2);
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    // Re-execute the frozen decision to exercise all three routes again.
    // Only the generic MUC effect may append again; direct ledger hits settle.
    let report = execute().await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(
        report
            .outcomes
            .iter()
            .all(|(_, outcome)| *outcome == ExternalOutcome::Done),
        "{report:?}"
    );
    assert_eq!(append_count(&sm, &target).await, 4);
    assert_eq!(fixture.count("sm_ingress_appends").await, 2);
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    assert!(
        terminalize_if_complete(&fixture.uow, decision.message_key.expect("key"))
            .await
            .expect("mixed obligations settle")
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_mixed_direct_muc_detached_context_isolation() {
    mixed_direct_muc_context_isolation(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_mixed_direct_muc_detached_context_isolation() {
    if let Some(fixture) = IngressFixture::postgres("mixed_direct_muc_context").await {
        mixed_direct_muc_context_isolation(fixture).await;
    }
}
