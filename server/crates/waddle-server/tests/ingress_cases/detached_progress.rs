use super::*;
use crate::detached_progress_support::*;
use waddle_server::ingress::{
    effects::delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
    ExternalEffect,
};
use waddle_xmpp::{ingress::EffectMessageIdentity, registry::ConnectionRegistry, Stanza};

#[tokio::test]
async fn detached_progress_restart_retry_sqlite() {
    restart_and_retry(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn detached_progress_restart_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dp_restart").await {
        restart_and_retry(fixture).await;
    }
}

async fn live_retry(fixture: IngressFixture) {
    use kameo::actor::Spawn;
    use waddle_server::ingress::{execute::execute_effects, Deps, ImmediateSink};
    use waddle_xmpp::registry::{RegisterUserResource, UserRegistryActor};
    let [a, b, _] = resources();
    let connections = ConnectionRegistry::new();
    let sm = registry(&fixture).await;
    attach(&sm, &a).await;
    let mut submission = fixture.submission(Some("detached-live"), "live canonical");
    route(&mut submission, &[a.clone(), b.clone()], 1);
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("first commit");
    execute(&fixture, &first, &connections, &sm).await;
    assert_pending(&fixture, 1, 0).await;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    connections.register_with_carbons(b.clone(), sender, false);
    let users = UserRegistryActor::spawn(UserRegistryActor::new());
    users
        .ask(RegisterUserResource {
            entry: connections.get_entry(&b).expect("live resource entry"),
            jid: b.clone(),
        })
        .await
        .expect("register live B");
    let mut provisional = submission.plan.sanitized_message.clone();
    provisional.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "live policy drift".to_owned(),
    );
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
            route_identity: Some(EffectMessageIdentity::capture_ordinal(1)),
            jid: b.clone(),
            stanza: Box::new(Stanza::Message(provisional)),
            kind: PeerDeliveryKind::PeerStanza,
            call_setup: None,
        }),
    ))];
    let retry = retry_decision(&fixture, &submission).await;
    assert_eq!(retry.external.len(), 1);
    assert_eq!(
        retry.arm_owned_receipts.len(),
        1,
        "live subset remains arm-owned"
    );
    let mut deps = Deps::new(&connections, "example.com");
    deps.sm_session_registry = Some(&sm);
    deps.user_registry = Some(&users);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(report.terminalization_failure.is_none(), "{report:?}");
    let Stanza::Message(delivered) = receiver.try_recv().expect("live B delivery").stanza else {
        panic!("message delivery");
    };
    let delivered: minidom::Element = delivered.into();
    let canonical: minidom::Element = submission.plan.sanitized_message.clone().into();
    assert_eq!(
        delivered, canonical,
        "live replay uses the canonical envelope"
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(queued(&sm, &a).await.unacked_stanzas.len(), 1);
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    drop(sm);
    fixture.close().await;
}

#[tokio::test]
async fn detached_progress_live_subset_retry_sqlite() {
    live_retry(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn detached_progress_live_subset_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dp_live").await {
        live_retry(fixture).await;
    }
}

async fn missing_recorded_resource(fixture: IngressFixture) {
    let [a, b, c] = resources();
    let connections = ConnectionRegistry::new();
    let sm = registry(&fixture).await;
    attach(&sm, &a).await;
    let mut submission = fixture.submission(Some("detached-abc"), "three resources");
    route(&mut submission, &[a.clone(), b.clone(), c], 1);
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("first commit");
    execute(&fixture, &first, &connections, &sm).await;
    assert_pending(&fixture, 1, 0).await;
    attach(&sm, &b).await;
    submission.plan.plan = vec![detached_effect(
        std::slice::from_ref(&b),
        EffectMessageIdentity::capture_ordinal(1),
        submission.plan.sanitized_message.clone(),
    )];
    let retry = retry_decision(&fixture, &submission).await;
    execute(&fixture, &retry, &connections, &sm).await;
    assert_pending(&fixture, 2, 0).await;
    assert_eq!(queued(&sm, &a).await.unacked_stanzas.len(), 1);
    assert_eq!(queued(&sm, &b).await.unacked_stanzas.len(), 1);
    let replay = retry_decision(&fixture, &submission).await;
    assert!(
        replay.external.is_empty(),
        "B completion cannot erase C's unresolved obligation"
    );
    assert_eq!(replay.receipts_pending.len(), 1);
    drop(sm);
    fixture.close().await;
}

#[tokio::test]
async fn detached_progress_recorded_c_unresolved_sqlite() {
    missing_recorded_resource(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn detached_progress_recorded_c_unresolved_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dp_missing_c").await {
        missing_recorded_resource(fixture).await;
    }
}

async fn audience_drift(fixture: IngressFixture) {
    let [a, b, c] = resources();
    let connections = ConnectionRegistry::new();
    let sm = registry(&fixture).await;
    attach(&sm, &a).await;
    let mut submission = fixture.submission(Some("detached-audience"), "frozen audience");
    route(&mut submission, &[a.clone(), b.clone()], 1);
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("first commit");
    execute(&fixture, &first, &connections, &sm).await;
    attach(&sm, &b).await;
    attach(&sm, &c).await;
    // Both the fresh intent and fresh effect now offer B+C instead of A+B.
    submission.plan.intents.clear();
    submission.plan.plan.clear();
    route(&mut submission, &[b.clone(), c.clone()], 1);
    let retry = retry_decision(&fixture, &submission).await;
    assert_eq!(retry.external.len(), 1);
    execute(&fixture, &retry, &connections, &sm).await;
    assert_eq!(queued(&sm, &a).await.unacked_stanzas.len(), 1);
    assert_eq!(queued(&sm, &b).await.unacked_stanzas.len(), 1);
    assert!(
        queued(&sm, &c).await.unacked_stanzas.is_empty(),
        "new audience cannot receive historical work"
    );
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    drop(sm);
    fixture.close().await;
}

#[tokio::test]
async fn detached_progress_audience_drift_sqlite() {
    audience_drift(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn detached_progress_audience_drift_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dp_drift").await {
        audience_drift(fixture).await;
    }
}

async fn independent_captures(fixture: IngressFixture) {
    let [a, b, c] = resources();
    let connections = ConnectionRegistry::new();
    let sm = registry(&fixture).await;
    attach(&sm, &a).await;
    let mut submission = fixture.submission(Some("detached-captures"), "independent captures");
    route(&mut submission, &[a.clone(), b.clone()], 1);
    route(&mut submission, &[a.clone(), c.clone()], 2);
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("first commit");
    execute(&fixture, &first, &connections, &sm).await;
    assert_pending(&fixture, 2, 0).await;
    assert_eq!(
        queued(&sm, &a).await.unacked_stanzas.len(),
        2,
        "one append for each capture sharing A"
    );
    attach(&sm, &b).await;
    let retry = retry_decision(&fixture, &submission).await;
    execute(&fixture, &retry, &connections, &sm).await;
    assert_pending(&fixture, 3, 1).await;
    attach(&sm, &c).await;
    let retry = retry_decision(&fixture, &submission).await;
    execute(&fixture, &retry, &connections, &sm).await;
    assert_eq!(queued(&sm, &a).await.unacked_stanzas.len(), 2);
    assert_eq!(queued(&sm, &b).await.unacked_stanzas.len(), 1);
    assert_eq!(queued(&sm, &c).await.unacked_stanzas.len(), 1);
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 4);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    drop(sm);
    fixture.close().await;
}

#[tokio::test]
async fn detached_progress_independent_captures_sqlite() {
    independent_captures(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn detached_progress_independent_captures_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dp_captures").await {
        independent_captures(fixture).await;
    }
}

async fn concurrent_snapshots(fixture: IngressFixture) {
    let [a, b, _] = resources();
    let connections = ConnectionRegistry::new();
    let sm = registry(&fixture).await;
    attach(&sm, &a).await;
    let mut submission = fixture.submission(Some("detached-concurrent"), "at least once");
    route(&mut submission, &[a.clone(), b.clone()], 1);
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("first commit");
    execute(&fixture, &first, &connections, &sm).await;
    assert_pending(&fixture, 1, 0).await;
    attach(&sm, &b).await;
    let one = retry_decision(&fixture, &submission).await;
    let two = retry_decision(&fixture, &submission).await;
    let barrier = tokio::sync::Barrier::new(2);
    tokio::join!(
        async {
            barrier.wait().await;
            execute(&fixture, &one, &connections, &sm).await;
        },
        async {
            barrier.wait().await;
            execute(&fixture, &two, &connections, &sm).await;
        },
    );
    assert_eq!(queued(&sm, &a).await.unacked_stanzas.len(), 1);
    assert_eq!(
        queued(&sm, &b).await.unacked_stanzas.len(),
        2,
        "both frozen decisions may append B before either observes the other's progress"
    );
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    drop(sm);
    fixture.close().await;
}

#[tokio::test]
async fn detached_progress_concurrent_decisions_sqlite() {
    concurrent_snapshots(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn detached_progress_concurrent_decisions_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dp_concurrent").await {
        concurrent_snapshots(fixture).await;
    }
}

#[tokio::test]
async fn detached_progress_cross_archive_retry_sqlite() {
    cross_archive_retry(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn detached_progress_cross_archive_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dp_cross_archive").await {
        cross_archive_retry(fixture).await;
    }
}
