use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::interpret::effects::{EffectSink, PlanSink};
use waddle_xmpp::{
    ingress::IngressEffectIntent, protocol::CarbonKind, registry::ConnectionRegistry,
};

async fn local_carbons_retry_on_remote_owner(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("carbons-owner-moved"), "carbon body");
    let owner = submission.sender.to_bare();
    let exclude = vec![submission.sender.clone()];
    let local_intent = IngressEffectIntent::Carbons {
        carbon_recipients: vec![owner.with_resource_str("laptop").expect("sibling")],
        excluded_source: submission.sender.clone(),
        kind: CarbonKind::Sent,
    };
    submission.plan.intents = vec![local_intent.clone()];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::Carbons {
            owner: owner.clone(),
            exclude: exclude.clone(),
            message: Box::new(submission.plan.sanitized_message.clone()),
            kind: CarbonKind::Sent,
        }),
    ))];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("local-owner commit");
    assert!(first.class.advances());
    let key = first.message_key.expect("canonical message");
    let receipt = crate::ingress::durable::receipt_key(&local_intent).expect("local receipt");
    EffectReceiptRepository::record_receipt_pooled(
        &fixture.db,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("confirm first local fanout");
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("completed local fanout"));

    // The same resource reconnects through a different socket node, so its
    // unchanged origin-id now produces a remote-owner carbon plan.
    submission.plan.intents = vec![IngressEffectIntent::RelayCarbons {
        owner: owner.clone(),
        exclude: exclude.clone(),
        kind: CarbonKind::Sent,
    }];
    let sink = PlanSink::new();
    sink.observe_sender(&submission.sender);
    sink.record(PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayCarbons {
            owner,
            exclude,
            message: Box::new(submission.plan.sanitized_message.clone()),
            kind: CarbonKind::Sent,
            origin: None,
        }),
    )));
    submission.plan.plan = sink.take().0;
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    for _ in 0..2 {
        let retry = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("same-origin remote retry");
        assert!(retry.class.advances());
        assert_eq!(retry.message_key, Some(key));
        assert!(retry.external.is_empty(), "unrecorded relay is filtered");
        assert!(retry.external_receipts.is_empty());
        let report = execute_effects(
            &fixture.uow,
            &fixture.db,
            &retry,
            &ImmediateSink,
            &deps,
            Duration::from_secs(1),
        )
        .await;
        assert!(report.outcomes.is_empty(), "no relay is executed");
        assert!(report.receipt_failures.is_empty());
    }
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_effect_intents").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_local_carbons_retry_on_remote_owner_filters_unrecorded_relay() {
    local_carbons_retry_on_remote_owner(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_local_carbons_retry_on_remote_owner_filters_unrecorded_relay() {
    if let Some(fixture) = IngressFixture::postgres("carbons_owner_moved").await {
        local_carbons_retry_on_remote_owner(fixture).await;
    }
}
