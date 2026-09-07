//! RFC 0018 §3: a capture identity receipts only its complete frozen fanout.
use super::*;
use crate::ingress::{
    commit::commit_submission, test_support::IngressFixture, IngressEffectCapture,
};
use crate::server::routes::{
    interpret::{effects::PlanSink, interpret},
    websocket::tests as socket_tests,
};
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent},
    protocol::OutboundEvent,
};

async fn direct_receipt(fixture: IngressFixture, partial: bool) {
    let state = socket_tests::create_test_websocket_state().await;
    let first: jid::FullJid = "juliet@example.com/phone".parse().expect("first");
    let second: jid::FullJid = "juliet@example.com/laptop".parse().expect("second");
    let (first_tx, mut first_rx) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &first, first_tx).await;
    let (second_tx, second_rx) = tokio::sync::mpsc::channel(8);
    if partial {
        socket_tests::register_test_connection(&state, &second, second_tx).await;
    }
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let mut deps = Deps::registry_only(&state.deps.protocol.connection_registry)
        .with_ingress_effect_capture(Some(capture.clone()));
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.message_dispatcher = Some(&state.deps.protocol.dispatcher);
    deps.mam_storage = Some(&state.deps.protocol.mam_storage);
    deps.effects = &sink;
    let mut submission = fixture.submission(Some("direct-receipt"), "online dm");
    let target: jid::Jid = if partial {
        first.to_bare().into()
    } else {
        first.clone().into()
    };
    let mut message = submission.plan.sanitized_message.clone();
    message.to = Some(target.clone());
    submission.target = if partial {
        waddle_xmpp::ingress::NormalizedTarget::Bare(first.to_bare())
    } else {
        waddle_xmpp::ingress::NormalizedTarget::Full(first.clone())
    };
    submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        &message,
        &waddle_xmpp::ingress::DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![submission.sender.to_bare()],
            stanza_lang: None,
        },
    )
    .expect("addressed DM digest");
    submission.plan.sanitized_message = message.clone();
    interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: target,
            stanza: Box::new(Stanza::Message(message)),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    let (plan, execution) = sink.take();
    submission.plan.plan = plan;
    submission.plan.room_execution = execution;
    submission.plan.intents = capture.snapshot().intents;
    let route_intent = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteDirect { .. }))
        .expect("direct route intent");
    assert!(
        matches!(route_intent, IngressEffectIntent::RouteDirect { route_identity: EffectMessageIdentity::CaptureOrdinal(_), fanout, .. } if fanout.len() == if partial { 2 } else { 1 })
    );
    let receipt = crate::ingress::durable::receipt_key(route_intent).expect("route receipt");
    assert!(first_rx.try_recv().is_err(), "Phase A sends nothing");
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit direct route");
    assert_eq!(
        decision
            .external_receipts
            .iter()
            .filter(|keys| keys.contains(&receipt))
            .count(),
        if partial { 2 } else { 1 }
    );
    drop(second_rx);
    deps.effects = &ImmediateSink;
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(first_rx.try_recv().is_ok(), "online peer received DM");
    assert!(report.receipt_failures.is_empty());
    let key = decision.message_key.expect("canonical message");
    let mut tx = fixture.uow.begin().await.expect("receipt transaction");
    assert_eq!(
        EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("receipt lookup"),
        !partial
    );
    tx.commit().await.expect("receipt read complete");
    assert_eq!(
        terminalize_if_complete(&fixture.uow, key)
            .await
            .expect("terminalize"),
        !partial
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_online_full_dm_receipts_direct_route_and_terminalizes() {
    direct_receipt(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn postgres_online_full_dm_receipts_direct_route_and_terminalizes() {
    if let Some(fixture) = IngressFixture::postgres("online_full_dm_receipt").await {
        direct_receipt(fixture, false).await;
    }
}
#[tokio::test]
async fn sqlite_partial_multi_resource_dm_keeps_direct_route_pending() {
    direct_receipt(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn postgres_partial_multi_resource_dm_keeps_direct_route_pending() {
    if let Some(fixture) = IngressFixture::postgres("partial_direct_receipt").await {
        direct_receipt(fixture, true).await;
    }
}

#[cfg(feature = "clustering")]
async fn relayed_direct_receipt(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("relayed-direct-receipt"), "remote online dm");
    let recipient: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let identity = EffectMessageIdentity::capture_ordinal(7);
    let intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.to_bare(),
        fanout: vec![recipient.clone()],
        route_identity: identity.clone(),
    };
    let effect = ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid {
        route_identity: Some(identity),
        origin: None,
        target: recipient,
        stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
        call_setup: None,
    });
    submission.plan.intents = vec![intent];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(effect.clone()))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit relay");
    assert_eq!(decision.external_receipts[0].len(), 1);
    // Phase C consumes this typed result only after the owner confirms delivery.
    let outcome = EffectOutcome::Delivery(FullJidDeliveryOutcome::Delivered);
    let proven = vec![proven_receipts(
        &effect,
        &outcome,
        &decision.external_receipts[0],
    )];
    let classified = classify_outcome(&effect, outcome, &mut Vec::new());
    let receipts = completed_receipts(&decision, &[(effect, classified)], &proven, 0);
    assert_eq!(receipts.len(), 1);
    let key = decision.message_key.expect("canonical message");
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("await delivery receipt"));
    for receipt in receipts {
        EffectReceiptRepository::record_receipt_pooled(
            &fixture.db,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("record remote delivery");
    }
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("terminalize confirmed relay"));
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn sqlite_relay_full_jid_delivery_receipts_direct_route_and_terminalizes() {
    relayed_direct_receipt(IngressFixture::sqlite().await).await;
}
#[cfg(feature = "clustering")]
#[tokio::test]
async fn postgres_relay_full_jid_delivery_receipts_direct_route_and_terminalizes() {
    if let Some(fixture) = IngressFixture::postgres("relayed_direct_receipt").await {
        relayed_direct_receipt(fixture).await;
    }
}
