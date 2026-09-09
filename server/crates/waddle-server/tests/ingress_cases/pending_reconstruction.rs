use super::*;
use std::{sync::Arc, time::Duration};
use waddle_server::{
    ingress::{
        effects::{
            delivery::{ExternalDeliveryEffect, PreparedOfflineNotification},
            PlanSuppressionPolicy,
        },
        execute::execute_effects,
        Deps, ExternalEffect, ExternalOutcome, ImmediateSink, IngressDecision,
    },
    notification_outbox::{NotificationCandidate, NotificationOutboxStore},
    pending_delivery::DatabasePendingDeliveryStorage,
};
use waddle_xmpp::{
    ingress::{
        NotificationActivityMutation, NotificationCandidateOutcome, PendingDeliveryMutation,
    },
    pending_delivery::{
        storage::PendingDeliveryStorage, PendingPayload, PendingRow, PendingRowId, QuotaPolicy,
    },
    registry::ConnectionRegistry,
};

#[path = "pending_reconstruction_support.rs"]
mod support;
use support::*;

async fn lost_execute(fixture: IngressFixture, archived: bool) {
    let storage = storage(&fixture, QuotaPolicy::Unlimited).await;
    let submission = pending_plan(&fixture, "pending-replay", archived);
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("initial commit");
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(&storage);
    execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::ZERO,
    )
    .await;
    assert_eq!(fixture.count("pending_delivery").await, 0);
    // Make the original acceptance observably older than replay/flush time.
    fixture
        .execute(
            "UPDATE ingress_messages SET created_at = '2020-01-02T03:04:05.123Z'",
            (),
        )
        .await;
    let fresh = fixture.submission(Some("pending-replay"), "canonical pending payload");
    let replay = commit_submission(&fixture.uow, &fresh, 5)
        .await
        .expect("reconstruct missing effect");
    assert_eq!(row(&replay).id, row(&first).id);
    assert_eq!(
        execute(&fixture, &storage, &replay).await,
        ExternalOutcome::Done
    );
    assert_eq!(
        storage
            .list(&row(&first).recipient)
            .await
            .expect("pending rows")
            .len(),
        1
    );
    complete(&fixture, archived).await;
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("canonical timestamp transaction");
    let created_at =
        CanonicalMessageRepository::created_at(&mut tx, first.message_key.expect("key"))
            .await
            .expect("canonical receipt time");
    tx.commit().await.expect("timestamp read");
    let stored = storage
        .list(&row(&first).recipient)
        .await
        .expect("persisted canonical row")
        .pop()
        .expect("one row");
    assert_eq!(
        stored.original_receipt_at.timestamp_millis(),
        created_at.timestamp_millis()
    );
    if !archived {
        use waddle_server::pending_delivery::{
            flush_for_resource, FlushContext, NullArchiveResolver,
        };
        let full = row(&first)
            .recipient
            .with_resource_str("reconnected")
            .expect("resource");
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        registry.register_with_carbons(full.clone(), sender, false);
        let flushed = flush_for_resource(
            &storage,
            &registry,
            &stored.recipient,
            &full,
            FlushContext {
                server_domain: "example.com",
                sm_session: None,
                blocking_storage: None,
                owner: None,
                archive_resolver: &NullArchiveResolver,
            },
        )
        .await;
        assert_eq!(flushed.pushed, 1);
        let waddle_xmpp::Stanza::Message(message) =
            receiver.try_recv().expect("flushed stanza").stanza
        else {
            panic!("message")
        };
        assert_eq!(message.bodies, submission.plan.sanitized_message.bodies);
        assert_eq!(message.from, submission.plan.sanitized_message.from);
        let delay = message
            .payloads
            .iter()
            .find(|payload| payload.is("delay", waddle_xmpp::xep::NS_DELAY))
            .expect("XEP-0203 delay");
        let timestamp = chrono::DateTime::parse_from_rfc3339(delay.attr("stamp").expect("stamp"))
            .expect("timestamp");
        // The existing XEP-0203 serializer emits whole-second timestamps.
        assert_eq!(timestamp.timestamp(), created_at.timestamp());
        assert_eq!(fixture.count("pending_delivery").await, 0);
        let fresh_after_flush = pending_plan(&fixture, "pending-replay", false);
        let settled = commit_submission(&fixture.uow, &fresh_after_flush, 5)
            .await
            .expect("completed replay after flush");
        assert!(
            settled.external.is_empty(),
            "completed replay cannot admit a fresh pending row"
        );
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &settled,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(fixture.count("pending_delivery").await, 0);
    }
    if archived {
        assert!(storage
            .list_unoutboxed_archived(20)
            .await
            .expect("janitor candidates")
            .is_empty());
    }
    fixture.close().await;
}

async fn existing_row(fixture: IngressFixture) {
    let storage = storage(&fixture, QuotaPolicy::CountCap { max_rows: 1 }).await;
    let submission = pending_plan(&fixture, "existing-row", false);
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    storage
        .insert(row(&decision).clone())
        .await
        .expect("earlier insert without receipt");
    assert_eq!(
        execute(&fixture, &storage, &decision).await,
        ExternalOutcome::Done
    );
    assert_eq!(fixture.count("pending_delivery").await, 1);
    complete(&fixture, false).await;
    fixture.close().await;
}

async fn policy_drift(fixture: IngressFixture, duplicate: bool) {
    let storage = storage(&fixture, QuotaPolicy::Unlimited).await;
    let original = pending_plan(&fixture, "policy-drift", true);
    let first = commit_submission(&fixture.uow, &original, 5)
        .await
        .expect("commit");
    if duplicate {
        let ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
            prepared_notification: PreparedOfflineNotification::Prepared(candidate),
            ..
        }) = &first.external[0]
        else {
            panic!("candidate")
        };
        NotificationOutboxStore::new(fixture.db.clone())
            .await
            .expect("outbox")
            .insert_candidate(candidate)
            .await
            .expect("janitor won");
    }
    let mut fresh = pending_plan(&fixture, "policy-drift", true);
    fresh.plan.intents.retain(|intent| {
        !matches!(
            intent,
            IngressEffectIntent::NotificationActivityPreview {
                mutation: NotificationActivityMutation::NotificationCandidate { .. },
                ..
            }
        )
    });
    let Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
        prepared_notification,
        ..
    })) = &mut fresh.plan.plan[0].effect
    else {
        panic!("effect")
    };
    *prepared_notification = PreparedOfflineNotification::Suppressed;
    let replay = commit_submission(&fixture.uow, &fresh, 5)
        .await
        .expect("policy drift replay");
    assert_eq!(row(&replay).id, row(&first).id);
    assert_eq!(
        execute(&fixture, &storage, &replay).await,
        ExternalOutcome::Done
    );
    assert_eq!(fixture.count("notification_candidates").await, 1);
    complete(&fixture, true).await;
    fixture.close().await;
}

async fn concurrent_replay(fixture: IngressFixture) {
    let storage = storage(&fixture, QuotaPolicy::Unlimited).await;
    let submission = pending_plan(&fixture, "concurrent-pending", false);
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    let second = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("replay");
    let (a, b) = tokio::join!(
        execute(&fixture, &storage, &first),
        execute(&fixture, &storage, &second)
    );
    assert_eq!(a, ExternalOutcome::Done);
    assert_eq!(b, ExternalOutcome::Done);
    assert_eq!(fixture.count("pending_delivery").await, 1);
    complete(&fixture, false).await;
    fixture.close().await;
}

macro_rules! dialect_tests {
    ($sqlite:ident, $postgres:ident, $test:ident $(, $argument:expr)?) => {
        #[tokio::test] async fn $sqlite() { $test(IngressFixture::sqlite().await $(, $argument)?).await; }
        #[tokio::test] async fn $postgres() { if let Some(fixture) = IngressFixture::postgres(stringify!($test)).await { $test(fixture $(, $argument)?).await; } }
    };
}
dialect_tests!(
    pending_lost_transient_sqlite,
    pending_lost_transient_postgres,
    lost_execute,
    false
);
dialect_tests!(
    pending_lost_archived_sqlite,
    pending_lost_archived_postgres,
    lost_execute,
    true
);
dialect_tests!(
    pending_existing_row_sqlite,
    pending_existing_row_postgres,
    existing_row
);
dialect_tests!(
    pending_policy_drift_sqlite,
    pending_policy_drift_postgres,
    policy_drift,
    false
);
dialect_tests!(
    pending_duplicate_candidate_sqlite,
    pending_duplicate_candidate_postgres,
    policy_drift,
    true
);
dialect_tests!(
    pending_concurrent_sqlite,
    pending_concurrent_postgres,
    concurrent_replay
);

async fn notification_only(fixture: IngressFixture) {
    use waddle_server::ingress_uow::EffectReceiptRepository;
    let storage = storage(&fixture, QuotaPolicy::Unlimited).await;
    let submission = pending_plan(&fixture, "notification-only", true);
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit");
    storage
        .insert(row(&first).clone())
        .await
        .expect("initial pending insert");
    let pending_kind = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::PendingDelivery { .. }))
        .expect("pending intent")
        .with_encoded_v1(|kind, _| kind)
        .expect("pending storage kind");
    let pending_key = first.external_receipts[0]
        .iter()
        .find(|key| key.kind.to_storage() == pending_kind)
        .expect("pending key");
    EffectReceiptRepository::record_receipt_pooled(
        &fixture.db,
        first.message_key.expect("canonical"),
        pending_key.kind,
        &pending_key.semantic_identity_hash,
    )
    .await
    .expect("pending receipt");
    use waddle_server::pending_delivery::{flush_for_resource, FlushContext, MamArchiveResolver};
    use waddle_xmpp::mam::{MamStorage, SqlxMamStorage};
    let mam = Arc::new(
        SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    let PendingPayload::Archived(stamp) = &row(&first).payload else {
        panic!("archived row")
    };
    let mut archived = ArchivedMessage::for_test(
        submission.sender.clone().into(),
        row(&first).recipient.clone().into(),
    );
    archived.id.clone_from(&stamp.id);
    archived.stanza_id = Some(stamp.clone());
    archived.stanza_xml = Some(String::from(&xmpp_parsers::minidom::Element::from(
        submission.plan.sanitized_message.clone(),
    )));
    mam.store_message(&row(&first).recipient, &archived)
        .await
        .expect("archived canonical payload");
    let registry = ConnectionRegistry::new();
    let full = row(&first)
        .recipient
        .with_resource_str("flush")
        .expect("resource");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    registry.register_with_carbons(full.clone(), sender, false);
    let flushed = flush_for_resource(
        &storage,
        &registry,
        &row(&first).recipient,
        &full,
        FlushContext {
            server_domain: "example.com",
            sm_session: None,
            blocking_storage: None,
            owner: None,
            archive_resolver: &MamArchiveResolver { mam_storage: mam },
        },
    )
    .await;
    assert_eq!(flushed.pushed, 1);
    assert!(
        receiver.try_recv().is_ok(),
        "row reached recovering connection"
    );
    let fresh = fixture.submission(Some("notification-only"), "canonical pending payload");
    let replay = commit_submission(&fixture.uow, &fresh, 5)
        .await
        .expect("notification repair");
    assert_eq!(
        execute(&fixture, &storage, &replay).await,
        ExternalOutcome::Done
    );
    assert_eq!(
        fixture.count("pending_delivery").await,
        0,
        "a flushed receipted row must never be reinserted"
    );
    assert_eq!(fixture.count("notification_candidates").await, 1);
    complete(&fixture, true).await;
    fixture.close().await;
}

async fn quota_bounce(fixture: IngressFixture) {
    let storage = storage(&fixture, QuotaPolicy::CountCap { max_rows: 1 }).await;
    let filler = commit_submission(
        &fixture.uow,
        &pending_plan(&fixture, "quota-first", false),
        5,
    )
    .await
    .expect("first commit");
    assert_eq!(
        execute(&fixture, &storage, &filler).await,
        ExternalOutcome::Done
    );
    let submission = pending_plan(&fixture, "quota-full", false);
    let full = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("full commit");
    let registry = ConnectionRegistry::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    registry.register_with_carbons(submission.sender.clone(), sender, false);
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(&storage);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &full,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Failed);
    let waddle_xmpp::Stanza::Message(bounce) = receiver
        .try_recv()
        .expect("service unavailable bounce")
        .stanza
    else {
        panic!("message bounce")
    };
    assert_eq!(bounce.type_, xmpp_parsers::message::MessageType::Error);
    assert_eq!(bounce.bodies, submission.plan.sanitized_message.bodies);
    assert_eq!(
        bounce
            .payloads
            .iter()
            .find_map(
                |payload| xmpp_parsers::stanza_error::StanzaError::try_from(payload.clone()).ok()
            )
            .expect("typed stanza error")
            .defined_condition,
        xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable
    );
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

dialect_tests!(
    pending_notification_only_sqlite,
    pending_notification_only_postgres,
    notification_only
);
dialect_tests!(
    pending_quota_bounce_sqlite,
    pending_quota_bounce_postgres,
    quota_bounce
);

async fn live_recipient(fixture: IngressFixture) {
    use waddle_server::ingress::effects::delivery::PeerDeliveryKind;
    use waddle_xmpp::ingress::EffectMessageIdentity;
    let storage = storage(&fixture, QuotaPolicy::Unlimited).await;
    let first = commit_submission(
        &fixture.uow,
        &pending_plan(&fixture, "live-pending", false),
        5,
    )
    .await
    .expect("offline commit");
    let mut fresh = fixture.submission(Some("live-pending"), "canonical pending payload");
    let full = "juliet@example.com/phone"
        .parse::<jid::FullJid>()
        .expect("live resource");
    let identity = EffectMessageIdentity::capture_ordinal(0);
    fresh.plan.intents.push(IngressEffectIntent::RouteDirect {
        recipient: full.to_bare(),
        fanout: vec![full.clone()],
        route_identity: identity.clone(),
    });
    fresh.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(identity),
                jid: full.clone(),
                stanza: Box::new(waddle_xmpp::Stanza::Message(
                    fresh.plan.sanitized_message.clone(),
                )),
                kind: PeerDeliveryKind::PeerStanza,
                call_setup: None,
            },
        )))
        .with_suppression(PlanSuppressionPolicy::SenderOnly),
    );
    let replay = commit_submission(&fixture.uow, &fresh, 5)
        .await
        .expect("live replay");
    assert_eq!(
        replay.external.len(),
        1,
        "recorded offline audience excludes fresh live route"
    );
    assert_eq!(row(&replay).id, row(&first).id);
    let registry = ConnectionRegistry::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    registry.register_with_carbons(full, sender, false);
    let mut deps = Deps::new(&registry, "example.com");
    deps.pending_delivery_storage = Some(&storage);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(receiver.try_recv().is_err(), "no live duplicate");
    assert_eq!(fixture.count("pending_delivery").await, 1);
    complete(&fixture, false).await;
    fixture.close().await;
}

async fn final_quota_race(fixture: IngressFixture) {
    let storage = storage(&fixture, QuotaPolicy::CountCap { max_rows: 1 }).await;
    let a = commit_submission(
        &fixture.uow,
        &pending_plan(&fixture, "quota-race-a", false),
        5,
    )
    .await
    .expect("first commit");
    let b = commit_submission(
        &fixture.uow,
        &pending_plan(&fixture, "quota-race-b", false),
        5,
    )
    .await
    .expect("second commit");
    let (a, b) = tokio::join!(
        execute(&fixture, &storage, &a),
        execute(&fixture, &storage, &b)
    );
    assert_eq!(
        [a, b]
            .iter()
            .filter(|outcome| **outcome == ExternalOutcome::Done)
            .count(),
        1
    );
    assert_eq!(
        [a, b]
            .iter()
            .filter(|outcome| **outcome == ExternalOutcome::Failed)
            .count(),
        1
    );
    assert_eq!(fixture.count("pending_delivery").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

dialect_tests!(
    pending_live_recipient_sqlite,
    pending_live_recipient_postgres,
    live_recipient
);
dialect_tests!(
    pending_final_quota_race_sqlite,
    pending_final_quota_race_postgres,
    final_quota_race
);

async fn payload_kind_drift(fixture: IngressFixture, archived: bool) {
    let storage = storage(&fixture, QuotaPolicy::Unlimited).await;
    let first = commit_submission(
        &fixture.uow,
        &pending_plan(&fixture, "payload-drift", archived),
        5,
    )
    .await
    .expect("canonical commit");
    let fresh = pending_plan(&fixture, "payload-drift", !archived);
    let replay = commit_submission(&fixture.uow, &fresh, 5)
        .await
        .expect("changed payload kind replay");
    assert_eq!(replay.external.len(), 1);
    assert_eq!(row(&replay).id, row(&first).id);
    assert_eq!(
        matches!(row(&replay).payload, PendingPayload::Archived(_)),
        archived
    );
    assert_eq!(
        execute(&fixture, &storage, &replay).await,
        ExternalOutcome::Done
    );
    assert_eq!(fixture.count("pending_delivery").await, 1);
    complete(&fixture, archived).await;
    fixture.close().await;
}

dialect_tests!(
    pending_archived_kind_drift_sqlite,
    pending_archived_kind_drift_postgres,
    payload_kind_drift,
    true
);
dialect_tests!(
    pending_transient_kind_drift_sqlite,
    pending_transient_kind_drift_postgres,
    payload_kind_drift,
    false
);
