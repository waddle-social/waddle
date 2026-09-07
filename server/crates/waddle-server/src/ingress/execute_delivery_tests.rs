use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::interpret::effects::{invite::MucUserRoute, EffectSink, PlanSink};
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent, PendingDeliveryMutation},
    pending_delivery::{PendingPayload, PendingRow, PendingRowId},
    protocol::CarbonKind,
    registry::ConnectionRegistry,
};

async fn invite_delivered(fixture: IngressFixture) {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let registry = ConnectionRegistry::new();
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let resource = recipient.with_resource_str("phone").expect("resource");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    registry.register(resource.clone(), tx);
    let mut submission = fixture.submission(Some("online-invite"), "invitation");
    let message = Box::new(submission.plan.sanitized_message.clone());
    let row_id = PendingRowId::fresh();
    let identity = EffectMessageIdentity::CaptureOrdinal(0);
    submission.plan.intents = vec![
        IngressEffectIntent::RouteDirect {
            recipient: recipient.clone(),
            fanout: vec![resource.clone()],
            route_identity: identity.clone(),
        },
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient: recipient.clone(),
                row_id: row_id.clone(),
            },
        },
    ];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::RouteToPeer(MucUserRoute {
            route_identity: Some(identity),
            recipient: recipient.clone(),
            resources: vec![resource],
            message: message.clone(),
            fallback: PendingRow {
                id: row_id,
                recipient,
                original_receipt_at: chrono::Utc::now(),
                payload: PendingPayload::Transient(message),
                flushed_in_session: None,
                outbound_sequence: None,
            },
            failure: None,
        }),
    ))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    assert_eq!(decision.external_receipts[0].len(), 2);
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
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
    assert!(rx.try_recv().is_ok(), "live invitation delivered");
    assert!(report.receipt_failures.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert!(terminalize_if_complete(
        &fixture.uow,
        decision.message_key.expect("canonical message")
    )
    .await
    .expect("terminalize"));
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_online_invite_receipts_live_route_and_fallback() {
    invite_delivered(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_online_invite_receipts_live_route_and_fallback() {
    if let Some(fixture) = IngressFixture::postgres("online_invite_receipts").await {
        invite_delivered(fixture).await;
    }
}

async fn remote_carbons_failure(fixture: IngressFixture) {
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let mut submission = fixture.submission(Some("remote-carbon"), "carbon body");
    let owner = submission.sender.to_bare();
    let exclude = vec![submission.sender.clone()];
    let intent = IngressEffectIntent::RelayCarbons {
        owner: owner.clone(),
        exclude: exclude.clone(),
        kind: CarbonKind::Sent,
    };
    let effect = ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
        owner,
        exclude,
        kind: CarbonKind::Sent,
        origin: None,
        message: Box::new(submission.plan.sanitized_message.clone()),
    });
    let sink = PlanSink::new();
    sink.observe_sender(&submission.sender);
    sink.record(PlannedEffect::new(Effect::External(effect.clone())));
    submission.plan.plan = sink.take().0;
    submission.plan.intents = vec![intent];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    assert_eq!(fixture.count("ingress_effect_intents").await, 1);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Failed);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    let key = decision.message_key.expect("canonical key");
    assert!(!terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("pending"));
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry");
    assert_eq!(retry.external.len(), 1, "unreceipted remote fanout retries");
    assert_eq!(retry.external_receipts[0].len(), 1);
    // The owner-reply proof is the same typed completion consumed by Phase C.
    let outcome = EffectOutcome::Delivery(FullJidDeliveryOutcome::Delivered);
    let proven = vec![proven_receipts(
        &effect,
        &outcome,
        &retry.external_receipts[0],
    )];
    let classified = classify_outcome(&effect, outcome, &mut Vec::new());
    let completed = completed_receipts(&retry, &[(effect, classified)], &proven, 0);
    assert_eq!(completed.len(), 1);
    for receipt in completed {
        EffectReceiptRepository::record_receipt_pooled(
            &fixture.db,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("owner reply receipt");
    }
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("terminalized"));
    let confirmed_retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("confirmed retry");
    let report = tokio::time::timeout(
        Duration::from_secs(2),
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &confirmed_retry,
            &ImmediateSink,
            &deps,
            Duration::from_secs(1),
        ),
    )
    .await
    .expect("confirmed retry finishes");
    assert_eq!(
        report.outcomes[0].1,
        ExternalOutcome::Done,
        "confirmed remote delivery is not attempted against unavailable bridge"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_remote_carbons_failure_retains_intent_and_retry_success_receipts() {
    remote_carbons_failure(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_remote_carbons_failure_retains_intent_and_retry_success_receipts() {
    if let Some(fixture) = IngressFixture::postgres("remote_carbon_receipts").await {
        remote_carbons_failure(fixture).await;
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn remote_carbons_planning_captures_owner_obligation_before_relay() {
    use crate::clustering::route_bridge::{
        OrderedRelayDeliveryBridge, RemoteResourceOriginSnapshot,
    };
    use crate::server::routes::interpret::{
        interpret, OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind,
    };
    use std::sync::Arc;
    let bridge = OrderedRelayDeliveryBridge::new(
        tokio_util::sync::CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    let state =
        crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
            crate::clustering::ClusteringHandles {
                ordered_relay_delivery_bridge: Some(bridge),
                ..Default::default()
            },
            Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        )
        .await;
    let registry = ConnectionRegistry::new();
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("source");
    let owner = sender.to_bare();
    let sink = PlanSink::new();
    sink.observe_sender(&sender);
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&state);
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    deps.ordered_relay_origin = Some(OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::RemoteResource(RemoteResourceOriginSnapshot {
            jid: sender.clone(),
            registration_id: serde_json::from_value(serde_json::json!(uuid::Uuid::new_v4()))
                .expect("registration id"),
            socket_generation: serde_json::from_value(serde_json::json!(1))
                .expect("socket generation"),
            user_owner: crate::clustering::NodeId::new("remote-owner".into()),
        }),
        sender_entity: waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::UserActor,
            owner.to_string(),
        ),
        inbound_sequence: 1,
        handoff: None,
    });
    let mut message =
        xmpp_parsers::message::Message::new(Some("juliet@example.com".parse().expect("recipient")));
    message.from = Some(sender.clone().into());
    interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::SendCarbons {
            owner: owner.clone(),
            message: Box::new(message),
            kind: CarbonKind::Sent,
            exclude: vec![sender.clone()],
        }],
        &deps,
    )
    .await;
    assert_eq!(
        capture.snapshot().intents,
        vec![IngressEffectIntent::RelayCarbons {
            owner,
            exclude: vec![sender],
            kind: CarbonKind::Sent
        }]
    );
    let plan = sink.take().0;
    assert_eq!(plan.len(), 1);
    assert!(matches!(
        plan[0].effect,
        Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RelayCarbons { .. }
        ))
    ));
    assert_eq!(
        plan[0].suppression,
        crate::server::routes::interpret::effects::PlanSuppressionPolicy::Always
    );
}
