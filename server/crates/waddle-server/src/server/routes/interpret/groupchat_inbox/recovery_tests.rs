use super::*;
use crate::ingress::{
    commit::commit_submission,
    effects::{Effect, PlanSink},
    execute::execute_effects,
    test_support::IngressFixture,
    ExternalEffect, IngressEffectCapture,
};
use std::sync::Arc;
use waddle_xmpp::ingress::{GroupchatNotificationRecoveryAction, NotificationActivityMutation};

async fn state_for(fixture: &IngressFixture) -> Arc<WebSocketState> {
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared pool");
    let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool),
        Arc::new(fixture.authority().await),
    )
    .await;
    let mut state = match Arc::try_unwrap(state) {
        Ok(state) => state,
        Err(_) => panic!("fresh state is uniquely owned"),
    };
    state.deps.protocol.notification_settings_projection = Arc::new(
        crate::notification_settings_projection::NotificationSettingsProjectionStore::new(
            fixture.db.clone(),
        ),
    );
    Arc::new(state)
}

async fn deferred_policy_restart(fixture: IngressFixture, deliver: bool) {
    crate::pubsub::DatabasePubSubStorage::open(Some(fixture.db.database_url()))
        .await
        .expect("projection schema");
    let state = state_for(&fixture).await;
    let mut submission = crate::ingress::recovery_tests::recovery_plan(&fixture);
    let recovery = submission
        .plan
        .plan
        .iter()
        .find_map(|planned| match &planned.effect {
            Effect::External(ExternalEffect::Room(
                super::effects::room::ExternalRoomEffect::NotificationCandidate {
                    recovery, ..
                },
            )) => recovery.clone(),
            _ => None,
        })
        .expect("planned recovery");
    // Public-group default suppresses ordinary messages; a private-group frozen
    // bit selects Always. No live policy actor is involved in either recovery.
    let mut recovery = recovery;
    recovery.room_members_only = deliver;
    for intent in &mut submission.plan.intents {
        if let IngressEffectIntent::GroupchatNotificationRecovery { mutation } = intent {
            mutation.room_members_only = deliver;
        }
    }
    for planned in &mut submission.plan.plan {
        if let Effect::Durable(crate::ingress::DurableEffect::Room(
            super::effects::room::DurableRoomEffect::ProjectGroupchatInbox {
                recovery: Some(stored),
                ..
            },
        )) = &mut planned.effect
        {
            stored.room_members_only = deliver;
        }
    }
    submission.plan.plan.retain(|planned| {
        !matches!(
            &planned.effect,
            Effect::External(ExternalEffect::Room(
                super::effects::room::ExternalRoomEffect::NotificationCandidate { .. }
            ))
        )
    });
    submission.plan.intents.retain(|intent| !matches!(intent,
        IngressEffectIntent::GroupchatNotificationRecovery { mutation } if mutation.action == GroupchatNotificationRecoveryAction::Completed
    ) && !matches!(intent, IngressEffectIntent::NotificationActivityPreview { mutation: NotificationActivityMutation::NotificationCandidate { .. }, .. }));
    fixture.execute("ALTER TABLE notification_settings_projection RENAME TO notification_settings_projection_unavailable", crate::db_params![]).await;
    let capture = IngressEffectCapture::new();
    let sink = PlanSink::new();
    let registry = waddle_xmpp::registry::ConnectionRegistry::new();
    let mut deps =
        Deps::new(&registry, "example.com").with_ingress_effect_capture(Some(capture.clone()));
    deps.effects = &sink;
    let outcome = insert_groupchat_notification_candidate(GroupchatNotificationCandidateSeed {
        deps: Some(&deps),
        recovery: Some(&recovery),
        state: &state,
        owner: &recovery.key.recipient,
        room: &recovery.key.room,
        message: &submission.plan.sanitized_message,
        sender_jid: recovery.sender_jid.clone(),
        thread_id: crate::notification_outbox::NotificationThreadId::root(),
        archive_stanza_id: recovery.key.archive_stanza_id.clone(),
        is_live_occupant: recovery.is_live_occupant,
        room_members_only: recovery.room_members_only,
        sender_can_broadcast_channel_mention: recovery.sender_can_broadcast_channel_mention,
    })
    .await;
    assert!(matches!(
        outcome,
        GroupchatNotificationCandidateQueueOutcome::RetryLater
    ));
    let captured = capture.snapshot().intents;
    assert!(
        matches!(captured.as_slice(), [IngressEffectIntent::GroupchatNotificationRecovery { mutation }] if mutation.action == GroupchatNotificationRecoveryAction::DeferredPolicy)
    );
    submission.plan.intents.extend(captured);
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit T0 error obligation");
    execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &super::effects::ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
    let restarted = state_for(&fixture).await;
    let retry = reconcile_groupchat_notification_candidates_for_sweep(&restarted, 10).await;
    assert_eq!(retry.completed, 0);
    assert!(retry.had_failure);
    assert_eq!(fixture.count("notification_candidates").await, 0);
    fixture.execute("ALTER TABLE notification_settings_projection_unavailable RENAME TO notification_settings_projection", crate::db_params![]).await;
    let settled = reconcile_groupchat_notification_candidates_for_sweep(&restarted, 10).await;
    assert_eq!(settled.completed, 1);
    assert!(!settled.had_failure);
    assert_eq!(
        fixture.count("notification_candidates").await,
        i64::from(deliver)
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    drop(restarted);
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn recovery_t0_error_restart_deliver_sqlite() {
    deferred_policy_restart(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn recovery_t0_error_restart_suppressed_sqlite() {
    deferred_policy_restart(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn recovery_t0_error_restart_deliver_postgres() {
    if let Some(fixture) = IngressFixture::postgres("t0_deliver").await {
        deferred_policy_restart(fixture, true).await;
    }
}
#[tokio::test]
async fn recovery_t0_error_restart_suppressed_postgres() {
    if let Some(fixture) = IngressFixture::postgres("t0_suppressed").await {
        deferred_policy_restart(fixture, false).await;
    }
}

async fn recorded_candidate_sweep(fixture: IngressFixture) {
    crate::pubsub::DatabasePubSubStorage::open(Some(fixture.db.database_url()))
        .await
        .expect("projection schema");
    let state = state_for(&fixture).await;
    let submission = crate::ingress::recovery_tests::recovery_plan(&fixture);
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit before lost execution");
    let candidate = match &decision.external[0] {
        ExternalEffect::Room(super::effects::room::ExternalRoomEffect::NotificationCandidate {
            candidate: Some(candidate),
            ..
        }) => candidate,
        _ => panic!("candidate arm"),
    };
    assert!(matches!(
        evaluate_recovery_policy(&state, candidate, candidate.conversation_jid(), false)
            .await
            .expect("today policy"),
        crate::notification_outbox::T1PushDispatchOutcome::Suppressed { .. }
    ));
    let archived = waddle_xmpp::mam::ArchivedMessage {
        ordinal: None,
        id: candidate.archive_stanza_id().id.clone(),
        stanza_id: Some(candidate.archive_stanza_id().clone()),
        body: Some("archive data must not be needed".to_owned()),
        message_type: xmpp_parsers::message::MessageType::Groupchat,
        ..waddle_xmpp::mam::ArchivedMessage::for_test(
            candidate.sender_jid().clone(),
            candidate.conversation_jid().clone().into(),
        )
    };
    state
        .deps
        .protocol
        .mam_storage
        .store_message(candidate.conversation_jid(), &archived)
        .await
        .expect("archived message");
    assert_eq!(fixture.count("mam_messages").await, 1);
    fixture
        .execute("DELETE FROM mam_messages", crate::db_params![])
        .await;
    let swept = reconcile_groupchat_notification_candidates_for_sweep(&state, 10).await;
    assert_eq!(swept.completed, 1);
    assert!(!swept.had_failure);
    assert_eq!(fixture.count("notification_candidates").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 4);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn recovery_recorded_candidate_actual_sweep_sqlite() {
    recorded_candidate_sweep(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn recovery_recorded_candidate_actual_sweep_postgres() {
    if let Some(fixture) = IngressFixture::postgres("recorded_actual_sweep").await {
        recorded_candidate_sweep(fixture).await;
    }
}
