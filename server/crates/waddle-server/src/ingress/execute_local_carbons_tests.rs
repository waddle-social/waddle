//! XEP-0280: each frozen carbon destination completes independently.
use super::*;
use crate::ingress::{
    commit::commit_submission, test_support::IngressFixture, IngressEffectCapture,
    IngressSubmission,
};
use crate::server::routes::interpret::{
    effects::{EffectSink, PlanSink},
    interpret,
};
use waddle_xmpp::{
    ingress::IngressEffectIntent,
    protocol::{CarbonKind, OutboundEvent},
    registry::ConnectionRegistry,
};

async fn plan_carbons(
    submission: &mut IngressSubmission,
    registry: &ConnectionRegistry,
    foreign_first: bool,
) {
    let sink = PlanSink::new();
    sink.observe_sender(&submission.sender);
    let capture = IngressEffectCapture::new();
    let mut deps = Deps::new(registry, "example.com");
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    let mut exclude = vec![submission.sender.clone()];
    if foreign_first {
        exclude.insert(
            0,
            "juliet@example.com/phone"
                .parse()
                .expect("foreign excluded resource"),
        );
    }
    interpret(
        vec![OutboundEvent::SendCarbons {
            owner: submission.sender.to_bare(),
            message: Box::new(submission.plan.sanitized_message.clone()),
            kind: CarbonKind::Sent,
            exclude,
        }],
        &deps,
    )
    .await;
    let (plan, room_execution) = sink.take();
    submission.plan.plan = plan;
    submission.plan.room_execution = room_execution;
    submission.plan.intents = capture.snapshot().intents;
}

async fn local_carbons_receipts(
    fixture: IngressFixture,
    audience_size: usize,
    partial: bool,
    foreign_first: bool,
) {
    let registry = ConnectionRegistry::new();
    let mut submission = fixture.submission(Some("local-carbons-receipts"), "ordinary DM");
    let (source_tx, mut source_rx) = tokio::sync::mpsc::channel(8);
    registry.register(submission.sender.clone(), source_tx);
    let resources = ["a", "b", "c"]
        .into_iter()
        .take(audience_size)
        .map(|resource| {
            submission
                .sender
                .to_bare()
                .with_resource_str(resource)
                .expect("resource")
        })
        .collect::<Vec<_>>();
    let mut receivers = Vec::new();
    for resource in &resources {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        registry.register(resource.clone(), sender);
        assert!(registry.set_carbons_enabled(resource, true));
        receivers.push(Some(receiver));
    }
    plan_carbons(&mut submission, &registry, foreign_first).await;
    assert_eq!(submission.plan.intents.len(), audience_size);
    assert!(submission.plan.intents.iter().all(|intent| matches!(intent,
        IngressEffectIntent::Carbons {carbon_recipients, ..} if carbon_recipients.len() == 1)));
    if audience_size == 0 {
        assert!(
            submission.plan.plan.is_empty(),
            "empty audience creates no effect"
        );
    }
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit carbons");
    assert!(first.class.advances());
    let key = first.message_key.expect("canonical message");
    assert_eq!(first.external_receipts.len(), audience_size);
    assert!(first.external_receipts.iter().all(|keys| keys.len() == 1));
    if partial {
        drop(receivers[1].take());
    }
    let deps = Deps::new(&registry, "example.com");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert!(report.terminalization_failure.is_none());
    for receiver in receivers.iter_mut().flatten() {
        assert!(
            receiver.try_recv().is_ok(),
            "every healthy destination receives its copy"
        );
        assert!(receiver.try_recv().is_err(), "one carbon per destination");
    }
    assert!(source_rx.try_recv().is_err(), "source is excluded");
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        if partial {
            audience_size - 1
        } else {
            audience_size
        } as i64
    );
    assert_eq!(
        terminalize_if_complete(&fixture.uow, key)
            .await
            .expect("terminalize"),
        !partial
    );
    if partial {
        assert!(report
            .outcomes
            .iter()
            .any(|(_, outcome)| *outcome != ExternalOutcome::Done));
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        registry.register(resources[1].clone(), sender);
        assert!(registry.set_carbons_enabled(&resources[1], true));
        receivers[1] = Some(receiver);
        let late = submission
            .sender
            .to_bare()
            .with_resource_str("late")
            .expect("late resource");
        let (late_tx, mut late_rx) = tokio::sync::mpsc::channel(8);
        registry.register(late.clone(), late_tx);
        assert!(registry.set_carbons_enabled(&late, true));
        plan_carbons(&mut submission, &registry, foreign_first).await;
        let retry = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("recommit same origin");
        assert!(retry.class.advances());
        assert_eq!(retry.message_key, Some(key));
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
        assert!(
            late_rx.try_recv().is_err(),
            "retry preserves the original audience"
        );
        assert!(report
            .outcomes
            .iter()
            .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
        for (index, receiver) in receivers.iter_mut().enumerate() {
            let receiver = receiver.as_mut().expect("connected resource");
            assert_eq!(
                receiver.try_recv().is_ok(),
                index == 1,
                "retry only missing destination"
            );
            assert!(receiver.try_recv().is_err());
        }
        assert!(terminalize_if_complete(&fixture.uow, key)
            .await
            .expect("completed retry"));
        assert_eq!(
            fixture.count("ingress_effect_receipts").await,
            audience_size as i64
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_single_resource_dm_skips_empty_carbons_and_terminalizes() {
    local_carbons_receipts(IngressFixture::sqlite().await, 0, false, false).await;
}
#[tokio::test]
async fn postgres_single_resource_dm_skips_empty_carbons_and_terminalizes() {
    if let Some(fixture) = IngressFixture::postgres("empty_carbons").await {
        local_carbons_receipts(fixture, 0, false, false).await;
    }
}
#[tokio::test]
async fn sqlite_multi_resource_dm_receipts_carbons_and_terminalizes() {
    local_carbons_receipts(IngressFixture::sqlite().await, 3, false, false).await;
}
#[tokio::test]
async fn postgres_multi_resource_dm_receipts_carbons_and_terminalizes() {
    if let Some(fixture) = IngressFixture::postgres("complete_carbons").await {
        local_carbons_receipts(fixture, 3, false, false).await;
    }
}
#[tokio::test]
async fn sqlite_partial_carbons_retry_delivers_only_missing_resource() {
    local_carbons_receipts(IngressFixture::sqlite().await, 3, true, false).await;
}
#[tokio::test]
async fn postgres_partial_carbons_retry_delivers_only_missing_resource() {
    if let Some(fixture) = IngressFixture::postgres("partial_carbons").await {
        local_carbons_receipts(fixture, 3, true, false).await;
    }
}

#[tokio::test]
async fn sqlite_carbons_foreign_first_exclusion_preserves_sender_receipts() {
    local_carbons_receipts(IngressFixture::sqlite().await, 3, false, true).await;
}
#[tokio::test]
async fn postgres_carbons_foreign_first_exclusion_preserves_sender_receipts() {
    if let Some(fixture) = IngressFixture::postgres("foreign_carbon_exclusion").await {
        local_carbons_receipts(fixture, 3, false, true).await;
    }
}
