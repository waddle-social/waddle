use crate::{
    ingress::{
        commit::commit_submission,
        effects::{
            delivery::{ExternalDeliveryEffect, PreparedOfflineNotification},
            Effect,
        },
        execute::execute_effects,
        test_support::IngressFixture,
        Deps, ExternalEffect, ExternalOutcome, ImmediateSink, PlannedEffect,
    },
    notification_outbox::NotificationOutboxStore,
    pending_delivery::DatabasePendingDeliveryStorage,
};
use std::{sync::Arc, time::Duration};
use waddle_xmpp::{
    ingress::{
        IngressEffectIntent, NotificationActivityMutation, NotificationCandidateOutcome,
        PendingDeliveryMutation,
    },
    pending_delivery::{
        storage::PendingDeliveryStorage, PendingPayload, PendingRow, PendingRowId, QuotaPolicy,
    },
    registry::ConnectionRegistry,
};

use crate::ingress::{IngressEffectCapture, IngressSubmission};
use crate::server::routes::interpret::{effects::PlanSink, interpret};
use waddle_xmpp::protocol::OutboundEvent;

async fn pending_storage(fixture: &IngressFixture) -> Arc<dyn PendingDeliveryStorage> {
    NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox schema");
    Arc::new(
        DatabasePendingDeliveryStorage::from_database(fixture.db.clone(), QuotaPolicy::Unlimited)
            .await
            .expect("pending schema"),
    )
}

fn archived_submission(fixture: &IngressFixture, origin: &str) -> (IngressSubmission, PendingRow) {
    let submission = fixture.submission(Some(origin), "offline hardening body");
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
            origin,
            recipient.into(),
        )),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    (submission, row)
}

fn obligations(row: &PendingRow) -> [IngressEffectIntent; 3] {
    let PendingPayload::Archived(stamp) = &row.payload else {
        panic!("archived row")
    };
    [
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Archived {
                recipient: row.recipient.clone(),
                row_id: row.id.clone(),
                archive_stanza_id: stamp.clone(),
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: row.recipient.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: row.recipient.clone(),
                archive_stanza_id: stamp.clone(),
                outcome: NotificationCandidateOutcome::Inserted,
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: row.recipient.clone(),
            mutation: NotificationActivityMutation::OfflineDelivery {
                conversation: row.recipient.clone(),
                archive_stanza_id: stamp.clone(),
            },
        },
    ]
}

fn add_effect(
    submission: &mut IngressSubmission,
    row: PendingRow,
    prepared_notification: PreparedOfflineNotification,
) {
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
                row,
                prepared_notification,
                original_message: Box::new(submission.plan.sanitized_message.clone()),
            }),
        )));
}

async fn deferred_policy(fixture: IngressFixture) {
    crate::pubsub::DatabasePubSubStorage::open(Some(fixture.db.database_url()))
        .await
        .expect("projection schema");
    let storage = pending_storage(&fixture).await;
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared pool");
    let standalone = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool), Arc::clone(&standalone.deps.protocol.ingress),
    ).await;
    let mut state = match Arc::try_unwrap(state) {
        Ok(state) => state,
        Err(_) => panic!("fresh state is uniquely owned"),
    };
    state.deps.protocol.notification_settings_projection = Arc::new(
        crate::notification_settings_projection::NotificationSettingsProjectionStore::new(
            fixture.db.clone(),
        ),
    );
    let state = Arc::new(state);
    let (mut submission, row) = archived_submission(&fixture, "offline-deferred-policy");
    // Force the real T0 evaluator's settings read to fail in either dialect.
    fixture
        .execute(
            "ALTER TABLE notification_settings_projection RENAME TO unavailable_settings",
            (),
        )
        .await;
    let registry = ConnectionRegistry::new();
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let mut deps =
        Deps::new(&registry, "example.com").with_ingress_effect_capture(Some(capture.clone()));
    deps.effects = &sink;
    deps.web_socket_state = Some(&state);
    deps.pending_delivery_storage = Some(&storage);
    interpret(
        vec![OutboundEvent::QueueOfflineDelivery {
            recipient: row.recipient.clone(),
            payload: row.payload.clone(),
            original_receipt_at: row.original_receipt_at,
            original_message: Box::new(submission.plan.sanitized_message.clone()),
        }],
        &deps,
    )
    .await;
    fixture
        .execute(
            "ALTER TABLE unavailable_settings RENAME TO notification_settings_projection",
            (),
        )
        .await;
    submission.plan.plan = sink.take().0;
    submission.plan.intents = capture.snapshot().intents;
    assert_eq!(
        submission.plan.intents.len(),
        1,
        "T0 retry captures only pending delivery"
    );
    assert!(matches!(
        &submission.plan.plan[0].effect,
        Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueOfflineDelivery {
                prepared_notification: PreparedOfflineNotification::RetryLater,
                ..
            }
        ))
    ));
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit deferred plan");
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(&storage);
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
    assert_eq!(
        fixture
            .count("pending_delivery WHERE notification_outboxed_at_ms IS NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(
        storage
            .list_unoutboxed_archived(10)
            .await
            .expect("janitor")
            .len(),
        1
    );
    let replay = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("duplicate replay");
    execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        fixture
            .count("pending_delivery WHERE notification_outboxed_at_ms IS NULL")
            .await,
        1
    );
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    // Exercise a first execution whose preparation drifts after commit: the
    // pending receipt is still absent, so the arm must honor marker ownership.
    let (mut markerless, markerless_row) =
        archived_submission(&fixture, "offline-markerless-suppression");
    let [pending, _, _] = obligations(&markerless_row);
    markerless.plan.intents.push(pending);
    add_effect(
        &mut markerless,
        markerless_row,
        PreparedOfflineNotification::RetryLater,
    );
    let mut markerless = commit_submission(&fixture.uow, &markerless, 5)
        .await
        .expect("commit pending only");
    if let ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
        prepared_notification,
        ..
    }) = &mut markerless.external[0]
    {
        *prepared_notification = PreparedOfflineNotification::Suppressed;
    }
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &markerless,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(
        storage
            .list_unoutboxed_archived(10)
            .await
            .expect("janitor after markerless suppression")
            .len(),
        2
    );
    let (mut suppressed, suppressed_row) =
        archived_submission(&fixture, "offline-suppressed-policy");
    let [pending, _, marker] = obligations(&suppressed_row);
    suppressed.plan.intents.extend([pending, marker]);
    add_effect(
        &mut suppressed,
        suppressed_row,
        PreparedOfflineNotification::Suppressed,
    );
    let suppressed = commit_submission(&fixture.uow, &suppressed, 5)
        .await
        .expect("commit suppression");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &suppressed,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(
        fixture
            .count("pending_delivery WHERE notification_outboxed_at_ms IS NOT NULL")
            .await,
        1
    );
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        3
    );
    drop(state);
    fixture.close().await;
}

async fn missing_pending_obligation(fixture: IngressFixture) {
    let storage = pending_storage(&fixture).await;
    let (mut submission, row) = archived_submission(&fixture, "offline-missing-pending");
    let [pending, _, marker] = obligations(&row);
    let pending_key = super::receipt_key(&pending).expect("pending receipt identity");
    submission.plan.intents.extend([pending, marker]);
    add_effect(
        &mut submission,
        row,
        PreparedOfflineNotification::Suppressed,
    );
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit valid pending plan");
    // Corrupt only the durable obligation after admission so the retained
    // effect reaches the arm with unmatched pending evidence.
    fixture
        .execute(
            "DELETE FROM ingress_effect_intents WHERE kind = ?",
            crate::db_params![i64::from(pending_key.kind.to_storage())],
        )
        .await;
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(&storage);
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
    assert_eq!(
        fixture.count("pending_delivery").await,
        0,
        "unmatched pending evidence must roll back insertion"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}

async fn invalid_candidate_restores_pending(fixture: IngressFixture) {
    let storage = pending_storage(&fixture).await;
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(&storage);
    for (origin, from) in [
        ("offline-no-sender", None),
        (
            "offline-bare-sender",
            Some("romeo@example.com".parse().expect("bare sender")),
        ),
    ] {
        let (mut submission, row) = archived_submission(&fixture, origin);
        submission.plan.sanitized_message.from = from;
        let [pending, candidate, marker] = obligations(&row);
        submission.plan.intents.extend([pending, candidate, marker]);
        // Simulate interruption between committing the obligations and executing effects.
        commit_submission(&fixture.uow, &submission, 5)
            .await
            .expect("first commit");
        submission.plan.intents.clear();
        let decision = commit_submission(&fixture.uow, &submission, 5)
            .await
            .expect("restore duplicate");
        assert!(matches!(
            &decision.external[0],
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
                prepared_notification: PreparedOfflineNotification::RetryLater,
                ..
            })
        ));
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
    }
    assert_eq!(
        fixture
            .count("pending_delivery WHERE notification_outboxed_at_ms IS NULL")
            .await,
        2
    );
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        2,
        "pending repairs settle independently of invalid candidates"
    );
    fixture.close().await;
}

#[tokio::test]
async fn offline_deferred_policy_sqlite() {
    deferred_policy(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn offline_deferred_policy_postgres() {
    if let Some(f) = IngressFixture::postgres("offline_deferred").await {
        deferred_policy(f).await;
    }
}
#[tokio::test]
async fn offline_missing_pending_obligation_sqlite() {
    missing_pending_obligation(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn offline_missing_pending_obligation_postgres() {
    if let Some(f) = IngressFixture::postgres("offline_missing_pending").await {
        missing_pending_obligation(f).await;
    }
}
#[tokio::test]
async fn offline_invalid_candidate_restores_pending_sqlite() {
    invalid_candidate_restores_pending(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn offline_invalid_candidate_restores_pending_postgres() {
    if let Some(f) = IngressFixture::postgres("offline_invalid_candidate").await {
        invalid_candidate_restores_pending(f).await;
    }
}
