use super::*;

pub(super) fn pending_plan(
    fixture: &IngressFixture,
    origin: &str,
    archived: bool,
) -> IngressSubmission {
    let mut submission = fixture.submission(Some(origin), "canonical pending payload");
    let recipient = "juliet@example.com"
        .parse::<jid::BareJid>()
        .expect("recipient");
    let stamp = StanzaId::new(origin, recipient.clone().into());
    let row_id = PendingRowId::fresh();
    let mutation = if archived {
        PendingDeliveryMutation::Archived {
            recipient: recipient.clone(),
            row_id: row_id.clone(),
            archive_stanza_id: stamp.clone(),
        }
    } else {
        PendingDeliveryMutation::Transient {
            recipient: recipient.clone(),
            row_id: row_id.clone(),
        }
    };
    submission
        .plan
        .intents
        .push(IngressEffectIntent::PendingDelivery { mutation });
    let prepared_notification = if archived {
        for mutation in [
            NotificationActivityMutation::NotificationCandidate {
                conversation: recipient.clone(),
                archive_stanza_id: stamp.clone(),
                outcome: NotificationCandidateOutcome::Inserted,
            },
            NotificationActivityMutation::OfflineDelivery {
                conversation: recipient.clone(),
                archive_stanza_id: stamp.clone(),
            },
        ] {
            submission
                .plan
                .intents
                .push(IngressEffectIntent::NotificationActivityPreview {
                    owner: recipient.clone(),
                    mutation,
                });
        }
        PreparedOfflineNotification::Prepared(Box::new(
            NotificationCandidate::direct_message(
                recipient.clone(),
                submission.sender.clone().into(),
                stamp.clone(),
                false,
            )
            .expect("candidate")
            .with_last_message_body(Some("canonical pending payload".to_owned())),
        ))
    } else {
        PreparedOfflineNotification::Suppressed
    };
    let row = PendingRow {
        id: row_id,
        recipient,
        original_receipt_at: chrono::Utc::now() - chrono::Duration::days(1),
        payload: if archived {
            PendingPayload::Archived(stamp)
        } else {
            PendingPayload::Transient(Box::new(submission.plan.sanitized_message.clone()))
        },
        flushed_in_session: None,
        outbound_sequence: None,
    };
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueOfflineDelivery {
                prepared_notification,
                row,
                original_message: Box::new(submission.plan.sanitized_message.clone()),
            },
        )))
        .with_suppression(PlanSuppressionPolicy::SenderOnly),
    );
    submission
}

pub(super) async fn storage(
    fixture: &IngressFixture,
    quota: QuotaPolicy,
) -> Arc<dyn PendingDeliveryStorage> {
    NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox schema");
    Arc::new(
        DatabasePendingDeliveryStorage::open(Some(fixture.db.database_url()), quota)
            .await
            .expect("pending schema"),
    )
}

pub(super) async fn execute(
    fixture: &IngressFixture,
    storage: &Arc<dyn PendingDeliveryStorage>,
    decision: &IngressDecision,
) -> ExternalOutcome {
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(storage);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(report.terminalization_failure.is_none(), "{report:?}");
    assert_eq!(report.outcomes.len(), 1, "{report:?}");
    report.outcomes[0].1
}

pub(super) fn row(decision: &IngressDecision) -> &PendingRow {
    let ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery { row, .. }) =
        &decision.external[0]
    else {
        panic!("offline effect")
    };
    row
}

pub(super) async fn complete(fixture: &IngressFixture, archived: bool) {
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        if archived { 3 } else { 1 }
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
}
