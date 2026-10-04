use super::*;
use crate::{
    ingress::{
        commit::commit_submission, receipt_key, recovery_rebuild::RecoveryInput,
        test_support::IngressFixture,
    },
    ingress_uow::SendClaim,
    server::routes::{
        interpret::{
            effects::ImmediateSink, reconcile_xep0357_notification_candidates_for_sweep,
            DeliveryExecutionContext,
        },
        websocket::{interpret_loop::build_interpret_deps, tests as socket_tests},
    },
};
use std::{sync::Arc, time::Duration};
use waddle_xmpp::{
    ingress::EffectMessageIdentity,
    ownership::NodeIdentity,
    xep::{
        xep0191::{BlockingStorage, InMemoryBlockingStorage},
        xep0334::{add_hint, Hint},
    },
};

#[derive(Clone, Copy)]
enum RecipientPolicy {
    Notify,
    BlockSender,
    NoStore,
    NoPermanentStore,
}

#[derive(Clone, Copy)]
enum CrashPhase {
    ClaimOnly,
    Started,
}

async fn recovered_start_notification(fixture: IngressFixture, policy: RecipientPolicy) {
    recovered_attempt_notification(fixture, policy, CrashPhase::Started).await;
}

async fn recovered_attempt_notification(
    fixture: IngressFixture,
    policy: RecipientPolicy,
    crash_phase: CrashPhase,
) {
    let state = socket_tests::create_test_websocket_state_with_durable_ingress(&fixture).await;
    let recipient: FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let blocking = Arc::new(InMemoryBlockingStorage::new());
    let blocking_storage: Arc<dyn BlockingStorage> = blocking.clone();
    let mut deps = build_interpret_deps(&state, None);
    deps.blocking_storage = Some(&blocking_storage);
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    let intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.to_bare(),
        fanout: vec![recipient.clone()],
        route_identity: EffectMessageIdentity::capture_ordinal(1),
    };
    let mut submission = fixture.submission(None, "recover the push after the sender pod dies");
    let hint = match policy {
        RecipientPolicy::NoStore => Some(Hint::NoStore),
        RecipientPolicy::NoPermanentStore => Some(Hint::NoPermanentStore),
        RecipientPolicy::Notify | RecipientPolicy::BlockSender => None,
    };
    if let Some(hint) = hint {
        add_hint(&mut submission.plan.sanitized_message, hint);
    }
    submission.plan.intents = vec![intent.clone()];
    if hint.is_none() {
        let stamp = waddle_xmpp_core::xep0359::StanzaId::new(
            "ambiguous-push-source",
            recipient.to_bare().into(),
        );
        let mut archived = waddle_xmpp::mam::ArchivedMessage::for_test(
            submission.sender.clone().into(),
            recipient.to_bare().into(),
        );
        archived.id = stamp.id.clone();
        archived.stanza_id = Some(stamp.clone());
        archived.body = submission
            .plan
            .sanitized_message
            .bodies
            .values()
            .next()
            .cloned();
        state
            .deps
            .protocol
            .mam_storage
            .store_message(&recipient.to_bare(), &archived)
            .await
            .expect("recipient archive committed before live delivery");
        submission
            .plan
            .intents
            .push(IngressEffectIntent::ArchiveAuthoritative {
                ordinal: None,
                archive: recipient.to_bare(),
                by: recipient.to_bare(),
                stanza_id: stamp,
                archived_at: chrono::Utc::now(),
            });
    }
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("canonical route");
    let key = decision.message_key.expect("canonical key");
    let progress = RouteProgress::from_intent(&intent, None, vec![])
        .expect("route progress")
        .expect("direct route");
    let obligation = SendObligation {
        message: key,
        receipt: receipt_key(&intent).expect("receipt"),
        recipient: recipient.clone(),
    };
    let mut tx = fixture.uow.begin().await.expect("start transaction");
    let SendClaim::Acquired(lease) = SendAttemptRepository::claim(
        &mut tx,
        &obligation,
        &NodeIdentity::local(),
        Duration::from_secs(60),
    )
    .await
    .expect("claim send") else {
        panic!("fresh send lease")
    };
    if matches!(crash_phase, CrashPhase::Started) {
        assert!(SendAttemptRepository::start(&mut tx, &lease)
            .await
            .expect("start send"));
    }
    tx.commit()
        .await
        .expect("persist attempt before the simulated process crash");
    if matches!(crash_phase, CrashPhase::ClaimOnly) {
        assert_eq!(
            fixture
                .count("ingress_send_attempts WHERE state = 0 AND recovered = 0")
                .await,
            1,
            "exercise the initial reservation, not a started or reclaimed send"
        );
    }
    if matches!(policy, RecipientPolicy::BlockSender) {
        blocking.set_blocklist(recipient.to_bare(), vec![submission.sender.to_bare()]);
    }
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("reload durable recovery input");
    let recorded = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("recorded intents");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("canonical envelope");
    let created_at = CanonicalMessageRepository::created_at(&mut tx, key)
        .await
        .expect("created at");
    tx.commit().await.expect("recovery snapshot");
    let rebuilt = recovery_rebuild::rebuild(RecoveryInput {
        key,
        envelope: &envelope,
        created_at,
        recorded: &recorded,
        unreceipted: std::slice::from_ref(&intent),
        route_progress: vec![progress],
        host_owned_resources: vec![],
        departed_occupants: vec![],
        blocked_recipients: &[],
    })
    .expect("rebuild original route after restart");
    assert!(
        !rebuilt.decision.external.is_empty(),
        "exercise the recovered route executor"
    );
    let report = crate::ingress::execute::execute_effects(
        &fixture.uow,
        &fixture.db,
        &rebuilt.decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        report.receipt_failures.is_empty(),
        "recovery receipt failures: {:?}",
        report.receipt_failures
    );
    let pending = &state.deps.protocol.pending_delivery_storage;
    let rows = pending
        .list(&recipient.to_bare())
        .await
        .expect("pending delivery");
    let stores = matches!(
        policy,
        RecipientPolicy::Notify | RecipientPolicy::NoPermanentStore
    );
    assert_eq!(rows.len(), usize::from(stores));
    if matches!(policy, RecipientPolicy::Notify) {
        assert!(matches!(rows[0].payload, PendingPayload::Archived(_)));
    }
    if matches!(policy, RecipientPolicy::NoPermanentStore) {
        assert!(matches!(rows[0].payload, PendingPayload::Transient(_)));
    }
    let sweep = reconcile_xep0357_notification_candidates_for_sweep(&state, 64).await;
    assert!(!sweep.had_failure);
    let expected_candidates = i64::from(matches!(policy, RecipientPolicy::Notify));
    assert_eq!(sweep.completed, expected_candidates as usize);
    assert_eq!(
        state
            .deps
            .protocol
            .notification_outbox
            .count_all_candidates()
            .await
            .expect("candidate count"),
        expected_candidates
    );
    assert!(pending
        .list_unoutboxed_archived(64)
        .await
        .expect("outboxed pending")
        .is_empty());
    for row in rows {
        assert_eq!(
            pending
                .delete_row(&row.id)
                .await
                .expect("consume pending delivery"),
            1
        );
    }
    // A stale recovery decision can survive another worker's settlement. It must
    // not recreate consumed pending custody or another notification candidate.
    let retry = crate::ingress::execute::execute_effects(
        &fixture.uow,
        &fixture.db,
        &rebuilt.decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(retry.receipt_failures.is_empty());
    assert!(pending
        .list(&recipient.to_bare())
        .await
        .expect("no recreated pending")
        .is_empty());
    let sweep = reconcile_xep0357_notification_candidates_for_sweep(&state, 64).await;
    assert!(!sweep.had_failure);
    assert_eq!(sweep.completed, 0);
    assert_eq!(
        state
            .deps
            .protocol
            .notification_outbox
            .count_all_candidates()
            .await
            .expect("stable candidate count"),
        expected_candidates
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_ambiguous_recovery_creates_one_push_candidate_after_pending_consumption() {
    recovered_start_notification(IngressFixture::sqlite().await, RecipientPolicy::Notify).await;
}

#[tokio::test]
async fn postgres_ambiguous_recovery_creates_one_push_candidate_after_pending_consumption() {
    if let Some(fixture) = IngressFixture::postgres("ambiguous_push_candidate").await {
        recovered_start_notification(fixture, RecipientPolicy::Notify).await;
    }
}

#[tokio::test]
async fn sqlite_expired_initial_claim_recovers_one_push_candidate_without_a_sink() {
    recovered_attempt_notification(
        IngressFixture::sqlite().await,
        RecipientPolicy::Notify,
        CrashPhase::ClaimOnly,
    )
    .await;
}

#[tokio::test]
async fn postgres_expired_initial_claim_recovers_one_push_candidate_without_a_sink() {
    if let Some(fixture) = IngressFixture::postgres("expired_initial_claim_push").await {
        recovered_attempt_notification(fixture, RecipientPolicy::Notify, CrashPhase::ClaimOnly)
            .await;
    }
}

#[tokio::test]
async fn sqlite_ambiguous_recovery_respects_current_recipient_blocklist() {
    recovered_start_notification(IngressFixture::sqlite().await, RecipientPolicy::BlockSender)
        .await;
}

#[tokio::test]
async fn sqlite_ambiguous_recovery_no_store_cannot_create_push_candidate() {
    recovered_start_notification(IngressFixture::sqlite().await, RecipientPolicy::NoStore).await;
}

#[tokio::test]
async fn sqlite_ambiguous_recovery_no_permanent_store_cannot_create_push_candidate() {
    recovered_start_notification(
        IngressFixture::sqlite().await,
        RecipientPolicy::NoPermanentStore,
    )
    .await;
}
