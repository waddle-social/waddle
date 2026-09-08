//! Recorded invitation authority survives a later silent block policy.
use super::*;

async fn policy_replay(fixture: IngressFixture, blocked: bool) {
    use crate::server::routes::interpret::effects::ImmediateSink;
    let mut state = shared_state(&fixture).await;
    let blocking = Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new());
    Arc::get_mut(&mut state)
        .expect("exclusive state")
        .deps
        .protocol
        .blocking_storage = blocking.clone();
    let (message, actor) = invite_room(&state, true).await;
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    crate::server::routes::websocket::tests::register_test_connection(&state, &resource, tx).await;
    let mut submission = submission(&fixture, invite_plan(&state, &message, true).await).await;
    let accepted = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("accepted");
    // No Phase C work ran. Fresh policy either suppresses the invitation or
    // targets a newly connected resource absent from recorded authority.
    let mut extra_receiver = None;
    if blocked {
        blocking.set_blocklist(
            recipient,
            vec!["romeo@example.com".parse().expect("sender")],
        );
    } else {
        let (tx, extra_rx) = tokio::sync::mpsc::channel(8);
        crate::server::routes::websocket::tests::register_test_connection(
            &state,
            &"juliet@example.com/tablet".parse().expect("extra resource"),
            tx,
        )
        .await;
        extra_receiver = Some(extra_rx);
    }
    submission.plan = invite_plan(&state, &message, true).await;
    assert!(submission.plan.rejection.is_none());
    if blocked {
        assert!(submission.plan.intents.is_empty());
    }
    let replay = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("recorded acceptance");
    assert_eq!(replay.message_key, accepted.message_key);
    let saved_route = accepted
        .external
        .iter()
        .find_map(|effect| match effect {
            ExternalEffect::RouteToPeer(route) => Some(route),
            _ => None,
        })
        .expect("accepted route");
    let replay_route = replay
        .external
        .iter()
        .find_map(|effect| match effect {
            ExternalEffect::RouteToPeer(route) => Some(route),
            _ => None,
        })
        .expect("reconstructed route");
    assert_eq!(replay_route.route_identity, saved_route.route_identity);
    assert_eq!(replay_route.resources, saved_route.resources);
    drop(extra_receiver);
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
    let report = crate::ingress::execute::execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(rx.try_recv().is_ok(), "recorded invitation delivered");
    assert!(crate::ingress::execute::terminalize_if_complete(
        &fixture.uow,
        replay.message_key.expect("canonical"),
    )
    .await
    .expect("terminalize"));
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        fixture.count("ingress_effect_intents").await
    );
    actor.kill();
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_group_dm_invite_blocked_policy_replay_sqlite() {
    policy_replay(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn ingress_group_dm_invite_blocked_policy_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("invite_blocked_replay").await {
        policy_replay(fixture, true).await;
    }
}

#[tokio::test]
async fn ingress_group_dm_invite_audience_policy_replay_sqlite() {
    policy_replay(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn ingress_group_dm_invite_audience_policy_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("invite_audience_replay").await {
        policy_replay(fixture, false).await;
    }
}

async fn empty_blocked_authority(fixture: IngressFixture) {
    let mut state = shared_state(&fixture).await;
    let blocking = Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new());
    Arc::get_mut(&mut state)
        .expect("exclusive state")
        .deps
        .protocol
        .blocking_storage = blocking.clone();
    let (message, actor) = invite_room(&state, true).await;
    let recipient = "juliet@example.com".parse().expect("invitee");
    blocking.set_blocklist(
        recipient,
        vec!["romeo@example.com".parse().expect("sender")],
    );
    let mut submission = submission(&fixture, invite_plan(&state, &message, true).await).await;
    let accepted = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("silent acceptance");
    assert_eq!(accepted.class, IngressDecisionClass::Accepted);
    assert!(
        accepted.external.is_empty(),
        "metadata must never execute as a fresh grant"
    );
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    blocking.set_blocklist("juliet@example.com".parse().expect("invitee"), vec![]);
    submission.plan = invite_plan(&state, &message, true).await;
    assert!(
        !submission.plan.intents.is_empty(),
        "current policy would invite"
    );
    let replay = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("empty recorded authority");
    assert_eq!(replay.message_key, accepted.message_key);
    assert!(replay.external.is_empty());
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    actor.kill();
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_group_dm_invite_empty_blocked_authority_sqlite() {
    empty_blocked_authority(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_group_dm_invite_empty_blocked_authority_postgres() {
    if let Some(fixture) = IngressFixture::postgres("invite_empty_authority").await {
        empty_blocked_authority(fixture).await;
    }
}
