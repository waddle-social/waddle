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
    notification_outbox::{NotificationCandidate, NotificationOutboxStore},
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

async fn rollback(fixture: IngressFixture) {
    let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(
        DatabasePendingDeliveryStorage::from_database(fixture.db.clone(), QuotaPolicy::Unlimited)
            .await
            .expect("pending schema"),
    );
    NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox schema");
    let mut submission = fixture.submission(Some("offline-atomic"), "canonical pending body");
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let stamp = waddle_xmpp_core::xep0359::StanzaId::new(
        "offline-atomic-archive",
        recipient.clone().into(),
    );
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Archived(stamp.clone()),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let candidate = NotificationCandidate::direct_message(
        recipient.clone(),
        "romeo@example.com/phone".parse().expect("sender"),
        stamp.clone(),
        false,
    )
    .expect("candidate");
    submission.plan.intents.extend([
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Archived {
                recipient: recipient.clone(),
                row_id: row.id.clone(),
                archive_stanza_id: stamp.clone(),
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: recipient.clone(),
                archive_stanza_id: stamp.clone(),
                outcome: NotificationCandidateOutcome::Inserted,
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::OfflineDelivery {
                conversation: recipient.clone(),
                archive_stanza_id: stamp,
            },
        },
    ]);
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
                row,
                prepared_notification: PreparedOfflineNotification::Prepared(Box::new(candidate)),
                original_message: Box::new(submission.plan.sanitized_message.clone()),
            }),
        )));
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit pending plan");
    super::execute_uow::fail_before_offline_settlement(decision.message_key.expect("canonical"));
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(&storage);
    let failed = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(failed.outcomes[0].1, ExternalOutcome::Failed);
    assert_eq!(fixture.count("pending_delivery").await, 0);
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    let retried = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(retried.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(fixture.count("notification_candidates").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert!(storage
        .list_unoutboxed_archived(10)
        .await
        .expect("janitor selection")
        .is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn offline_atomic_rollback_sqlite() {
    rollback(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn offline_atomic_rollback_postgres() {
    if let Some(fixture) = IngressFixture::postgres("offline_atomic_rollback").await {
        rollback(fixture).await;
    }
}

async fn stale_completed_decision(fixture: IngressFixture) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(
        DatabasePendingDeliveryStorage::from_database(fixture.db.clone(), QuotaPolicy::Unlimited)
            .await
            .expect("pending schema"),
    );
    let mut submission = fixture.submission(Some("offline-stale-complete"), "canonical transient");
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Transient(Box::new(submission.plan.sanitized_message.clone())),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    submission
        .plan
        .intents
        .push(IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient,
                row_id: row.id.clone(),
            },
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
                row,
                prepared_notification: PreparedOfflineNotification::Suppressed,
                original_message: Box::new(submission.plan.sanitized_message.clone()),
            }),
        )));
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit pending plan");
    assert_eq!(decision.receipts_pending.len(), 1);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(&storage);
    let baseline = metrics
        .counter_sum("ingress.effects.unresolved", &[("kind", "delivery")])
        .unwrap_or(0);
    // Reuse the original snapshot as two concurrently admitted executions can:
    // the second arm sees AlreadyReceipted and has no new receipt to report.
    for _ in 0..2 {
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
        assert!(report.receipt_failures.is_empty());
        assert!(report.terminalization_failure.is_none());
        drop(report);
        assert_eq!(
            metrics
                .counter_sum("ingress.effects.unresolved", &[("kind", "delivery")])
                .unwrap_or(0),
            baseline,
            "transactionally complete stale decisions must not report unresolved delivery"
        );
    }
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn offline_stale_completed_decision_sqlite() {
    stale_completed_decision(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn offline_stale_completed_decision_postgres() {
    if let Some(fixture) = IngressFixture::postgres("offline_stale_completed").await {
        stale_completed_decision(fixture).await;
    }
}

struct QuotaEnvironment {
    registry: Arc<ConnectionRegistry>,
    storage: Arc<dyn PendingDeliveryStorage>,
}

impl crate::ingress::RecoveryEnvironment for QuotaEnvironment {
    fn recovery_deps(&self) -> Deps<'_> {
        let mut deps = Deps::new(&self.registry, "example.com");
        deps.pending_delivery_storage = Some(&self.storage);
        deps
    }
}

async fn quota_refusal(fixture: IngressFixture, capacity_returns: bool) {
    use crate::ingress::{
        maintenance::{run_maintenance_pass, MaintenanceBudget, MaintenanceOutcome},
        RecoveryEnvironment,
    };
    use crate::ingress_uow::{CanonicalMessageRepository, EffectReceiptRepository};

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let baseline = metrics
        .counter_sum("ingress.effects.unresolved", &[("kind", "delivery")])
        .unwrap_or(0);
    let registry = Arc::new(ConnectionRegistry::new());
    let mut environment = Arc::new(QuotaEnvironment {
        registry: registry.clone(),
        storage: Arc::new(
            DatabasePendingDeliveryStorage::from_database(
                fixture.db.clone(),
                QuotaPolicy::CountCap { max_rows: 0 },
            )
            .await
            .expect("pending schema"),
        ),
    });
    NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox schema");
    let mut submission =
        fixture.submission(Some("offline-quota-refusal"), "canonical pending body");
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let stamp =
        waddle_xmpp_core::xep0359::StanzaId::new("offline-quota-archive", recipient.clone().into());
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Archived(stamp.clone()),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let candidate = NotificationCandidate::direct_message(
        recipient.clone(),
        "romeo@example.com/phone".parse().expect("sender"),
        stamp.clone(),
        false,
    )
    .expect("candidate");
    submission.plan.intents.extend([
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Archived {
                recipient: recipient.clone(),
                row_id: row.id.clone(),
                archive_stanza_id: stamp.clone(),
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: recipient.clone(),
                archive_stanza_id: stamp.clone(),
                outcome: NotificationCandidateOutcome::Inserted,
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::OfflineDelivery {
                conversation: recipient.clone(),
                archive_stanza_id: stamp,
            },
        },
    ]);
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueOfflineDelivery {
                row,
                prepared_notification: PreparedOfflineNotification::Prepared(Box::new(candidate)),
                original_message: Box::new(submission.plan.sanitized_message.clone()),
            },
        )))
        // Match effects::delivery::record's offline replay policy.
        .with_suppression(crate::ingress::effects::PlanSuppressionPolicy::SenderOnly),
    );

    let (sender, mut frames) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(submission.sender.clone(), sender, false);
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit pending plan");
    let key = decision.message_key.expect("canonical key");
    assert_eq!(decision.receipts_pending.len(), 3);
    assert_eq!(decision.external_receipts[0].len(), 3);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &environment.recovery_deps(),
        Duration::from_secs(5),
    )
    .await;
    let frame = frames.try_recv().expect("quota bounce");
    let waddle_xmpp::Stanza::Message(bounce) = frame.stanza else {
        panic!("expected message bounce");
    };
    assert_eq!(bounce.type_, xmpp_parsers::message::MessageType::Error);
    assert_eq!(bounce.to, Some(submission.sender.clone().into()));
    assert!(bounce.payloads.iter().any(|payload| {
        xmpp_parsers::stanza_error::StanzaError::try_from(payload.clone()).is_ok_and(|error| {
            error.defined_condition
                == xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable
        })
    }));
    assert!(frames.try_recv().is_err());
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(report.receipt_failures.is_empty());
    assert!(report.terminalization_failure.is_none());
    assert_eq!(fixture.count("pending_delivery").await, 0);
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    let mut tx = fixture.uow.begin().await.expect("inspect refusal");
    assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("complete receipts"));
    assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal refusal"));
    tx.commit().await.expect("inspection commit");

    if capacity_returns {
        environment = Arc::new(QuotaEnvironment {
            registry,
            storage: Arc::new(
                DatabasePendingDeliveryStorage::from_database(
                    fixture.db.clone(),
                    QuotaPolicy::Unlimited,
                )
                .await
                .expect("unlimited pending storage"),
            ),
        });
    }
    // Reusing a stale decision must not produce even notification side effects.
    let retried = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &environment.recovery_deps(),
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(retried.outcomes[0].1, ExternalOutcome::Done);
    assert!(retried.receipt_failures.is_empty());
    assert!(retried.terminalization_failure.is_none());
    assert_eq!(fixture.count("pending_delivery").await, 0);
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert!(frames.try_recv().is_err());

    // Alias replay exercises recorded-offline restoration and suppression with
    // a fresh plan whose owned obligations have all already been receipted.
    let replay = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("alias replay");
    assert_eq!(replay.message_key, Some(key));
    assert!(replay.receipts_pending.is_empty());
    assert!(
        replay.external.is_empty(),
        "receipted pending row is not rebuilt"
    );
    let replayed = execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &environment.recovery_deps(),
        Duration::from_secs(5),
    )
    .await;
    assert!(replayed.receipt_failures.is_empty());
    for _ in 0..2 {
        assert_eq!(
            run_maintenance_pass(
                &fixture.db,
                &fixture.uow,
                MaintenanceBudget {
                    grace: chrono::Duration::zero(),
                    ..MaintenanceBudget::DEFAULT
                },
                Some(environment.clone()),
            )
            .await,
            MaintenanceOutcome::Complete
        );
        assert_eq!(fixture.count("pending_delivery").await, 0);
        assert_eq!(fixture.count("notification_candidates").await, 0);
        assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
        assert!(frames.try_recv().is_err(), "refusal bounce must not repeat");
    }
    assert_eq!(
        metrics
            .counter_sum("ingress.effects.unresolved", &[("kind", "delivery")])
            .unwrap_or(0),
        baseline
    );
    fixture.close().await;
}

#[tokio::test]
async fn quota_refusal_settles_receipts_and_bounces_once_sqlite() {
    quota_refusal(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn quota_refusal_settles_receipts_and_bounces_once_postgres() {
    if let Some(fixture) = IngressFixture::postgres("quota_refusal_bounces_once").await {
        quota_refusal(fixture, false).await;
    }
}

#[tokio::test]
async fn quota_refusal_is_not_recovered_when_capacity_returns_sqlite() {
    quota_refusal(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn quota_refusal_is_not_recovered_when_capacity_returns_postgres() {
    if let Some(fixture) = IngressFixture::postgres("quota_refusal_capacity_returns").await {
        quota_refusal(fixture, true).await;
    }
}
