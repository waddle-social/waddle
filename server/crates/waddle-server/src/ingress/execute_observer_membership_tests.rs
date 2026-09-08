use super::*;
use std::sync::Arc;
use waddle_extensions::{
    observer_test_support::{ObserverTestBehavior, ObserverTestPlugin},
    ExtensionManager, PluginId,
};

fn plugin(name: &str) -> Arc<ObserverTestPlugin> {
    ObserverTestPlugin::new(
        PluginId::new(name).expect("plugin id"),
        ObserverTestBehavior::Success,
    )
}

pub(super) fn select_observers(
    submission: &mut crate::ingress::IngressSubmission,
    manager: &ExtensionManager,
) {
    let effects = manager
        .message_observer_plugins(&submission.plan.sanitized_message)
        .into_iter()
        .map(|plugin| ExternalRoomEffect::ObserveRoomMessage {
            room: "room@muc.example.com".parse().expect("room"),
            plugin,
            message: Box::new(submission.plan.sanitized_message.clone()),
            requester: submission.sender.to_bare(),
            sender: submission.sender.clone(),
            error_request: Box::new(submission.plan.sanitized_message.clone()),
        })
        .collect::<Vec<_>>();
    submission.plan.intents = effects.iter().map(observer_intent).collect();
    submission.plan.plan = effects
        .into_iter()
        .map(|effect| {
            PlannedEffect::new(Effect::External(ExternalEffect::Room(effect)))
                .with_suppression(PlanSuppressionPolicy::Always)
        })
        .collect();
}

async fn execute_with_manager(
    fixture: &IngressFixture,
    decision: &crate::ingress::IngressDecision,
    manager: ExtensionManager,
) -> ExecutionReport {
    let mut state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    Arc::get_mut(&mut state)
        .expect("unique test state")
        .deps
        .protocol
        .extension_manager = Arc::new(manager);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(state.as_ref());
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert!(report.terminalization_failure.is_none());
    report
}

async fn frozen_membership(fixture: IngressFixture) {
    let a = plugin("observer-a");
    let b = plugin("observer-b");
    let c = plugin("observer-c");
    let original = ExtensionManager::with_observer_test_plugins(vec![a.clone(), b.clone()]).await;
    let mut submission = fixture.submission(Some("observer-membership"), "canonical body");
    select_observers(&mut submission, &original);
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("first commit");
    assert_eq!(first.external.len(), 2);

    let fresh = ExtensionManager::with_observer_test_plugins(vec![a.clone(), c.clone()]).await;
    submission
        .plan
        .sanitized_message
        .bodies
        .insert(Default::default(), "fresh body".into());
    select_observers(&mut submission, &fresh);
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("replay commit");
    assert_eq!(
        replay.class,
        crate::ingress::IngressDecisionClass::ExistingDivergent
    );
    assert_eq!(replay.message_key, first.message_key);
    assert_eq!(
        fixture.count("ingress_effect_intents").await,
        2,
        "C cannot insert an obligation"
    );
    assert_eq!(
        replay.external.len(),
        2,
        "B is reconstructed and C is dropped"
    );
    let plugins = replay
        .external
        .iter()
        .map(|effect| {
            let ExternalEffect::Room(ExternalRoomEffect::ObserveRoomMessage {
                plugin,
                message,
                error_request,
                ..
            }) = effect
            else {
                panic!("observer")
            };
            assert_eq!(
                message.bodies.values().next().expect("body"),
                "canonical body"
            );
            assert_eq!(error_request.bodies, message.bodies);
            plugin.as_str()
        })
        .collect::<Vec<_>>();
    assert!(plugins.contains(&"observer-a"));
    assert!(plugins.contains(&"observer-b"));
    let report = execute_with_manager(&fixture, &replay, fresh).await;
    assert_eq!(
        report
            .outcomes
            .iter()
            .filter(|(_, outcome)| *outcome == ExternalOutcome::Done)
            .count(),
        1
    );
    assert_eq!(
        report
            .outcomes
            .iter()
            .filter(|(_, outcome)| *outcome == ExternalOutcome::Failed)
            .count(),
        1
    );
    assert_eq!(a.invocations()[0].body.as_str(), "canonical body");
    assert!(
        b.invocations().is_empty(),
        "missing plugin remains unresolved"
    );
    assert!(c.invocations().is_empty(), "unrecorded plugin never runs");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);

    // The missing member becomes available after restart; successful A is skipped.
    let restored =
        ExtensionManager::with_observer_test_plugins(vec![a.clone(), b.clone(), c.clone()]).await;
    select_observers(&mut submission, &restored);
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry missing member");
    let report = execute_with_manager(&fixture, &retry, restored).await;
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    assert_eq!(a.invocations().len(), 1);
    assert_eq!(b.invocations().len(), 1);
    assert_eq!(b.invocations()[0].body.as_str(), "canonical body");
    assert!(c.invocations().is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

async fn zero_membership(fixture: IngressFixture) {
    let empty = ExtensionManager::with_observer_test_plugins(Vec::new()).await;
    let mut submission = fixture.submission(Some("observer-zero-membership"), "canonical body");
    select_observers(&mut submission, &empty);
    assert!(submission.plan.intents.is_empty());
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("empty acceptance");
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    let a = plugin("observer-a");
    let installed = ExtensionManager::with_observer_test_plugins(vec![a.clone()]).await;
    select_observers(&mut submission, &installed);
    assert_eq!(
        submission.plan.intents.len(),
        1,
        "new plugin is eligible now"
    );
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("empty authority replay");
    assert_eq!(
        replay.class,
        crate::ingress::IngressDecisionClass::ExistingDivergent
    );
    assert_eq!(replay.message_key, first.message_key);
    assert!(replay.external.is_empty());
    let report = execute_with_manager(&fixture, &replay, installed).await;
    assert!(report.outcomes.is_empty());
    assert!(a.invocations().is_empty());
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}

async fn revoked_membership(fixture: IngressFixture) {
    let a = plugin("observer-a");
    let manager = ExtensionManager::with_observer_test_plugins(vec![a.clone()]).await;
    let mut submission = fixture.submission(Some("observer-revoked"), "canonical body");
    select_observers(&mut submission, &manager);
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("record plugin");
    a.revoke();
    select_observers(&mut submission, &manager);
    assert!(submission.plan.plan.is_empty());
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("reconstruct revoked observer");
    assert_eq!(replay.external.len(), 1);
    let report = execute_with_manager(&fixture, &replay, manager).await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Failed);
    assert!(a.invocations().is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(
        !terminalize_if_complete(&fixture.uow, first.message_key.expect("key"))
            .await
            .expect("pending")
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_observer_frozen_membership_restores_missing_plugin() {
    frozen_membership(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_observer_frozen_membership_restores_missing_plugin() {
    if let Some(fixture) = IngressFixture::postgres("observer_membership").await {
        frozen_membership(fixture).await;
    }
}
#[tokio::test]
async fn sqlite_observer_zero_membership_stays_empty_after_install() {
    zero_membership(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_observer_zero_membership_stays_empty_after_install() {
    if let Some(fixture) = IngressFixture::postgres("observer_zero").await {
        zero_membership(fixture).await;
    }
}
#[tokio::test]
async fn sqlite_observer_revoked_grant_at_retry_stays_unresolved() {
    revoked_membership(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_observer_revoked_grant_at_retry_stays_unresolved() {
    if let Some(fixture) = IngressFixture::postgres("observer_revoked").await {
        revoked_membership(fixture).await;
    }
}

#[path = "execute_observer_xep0045_tests.rs"]
mod xep0045;
