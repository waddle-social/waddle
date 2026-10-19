use super::*;
use crate::{
    ingress::{
        commit::commit_submission, receipt_key, recovery_rebuild::RecoveryInput,
        test_support::IngressFixture,
    },
    ingress_uow::{
        ArchiveDispatchObligation, ArchiveDispatchRepository, DispatchTarget, SendClaim,
    },
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

#[derive(Clone, Copy)]
enum RecoveryTiming {
    Immediate,
    Delayed,
    DelayedUnknownFanout,
    DelayedMissingBridge,
}

async fn recovered_start_notification(fixture: IngressFixture, policy: RecipientPolicy) {
    recovered_attempt_notification(fixture, policy, CrashPhase::Started).await;
}

async fn recovered_attempt_notification(
    fixture: IngressFixture,
    policy: RecipientPolicy,
    crash_phase: CrashPhase,
) {
    recovered_attempt_notification_with_delay(
        fixture,
        policy,
        crash_phase,
        RecoveryTiming::Immediate,
    )
    .await;
}

async fn recovered_attempt_notification_with_delay(
    fixture: IngressFixture,
    policy: RecipientPolicy,
    crash_phase: CrashPhase,
    timing: RecoveryTiming,
) {
    let delay_recovery = !matches!(timing, RecoveryTiming::Immediate);
    let state = socket_tests::create_test_websocket_state_with_durable_ingress(&fixture).await;
    assert_eq!(
        state
            .deps
            .protocol
            .pending_delivery_storage
            .notification_custody_mode(),
        waddle_xmpp::pending_delivery::storage::PendingNotificationCustodyMode::CanonicalRequired,
        "ambiguous handoff recovery must exercise the strict durable backend"
    );
    let recipient: FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let blocking = Arc::new(InMemoryBlockingStorage::new());
    let blocking_storage: Arc<dyn BlockingStorage> = blocking.clone();
    let mut deps = build_interpret_deps(&state, None);
    deps.blocking_storage = Some(&blocking_storage);
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    let intent = IngressEffectIntent::RouteDirect {
        prepared: None,
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
    let mut archive_ordinal = None;
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
        archive_ordinal = state
            .deps
            .protocol
            .mam_storage
            .get_message_by_archive_or_stanza_id(&recipient.to_bare(), stamp.as_str())
            .await
            .expect("stored copy")
            .expect("real archive row")
            .ordinal;
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
    if delay_recovery {
        let archive = submission
            .plan
            .intents
            .iter()
            .find(|intent| matches!(intent, IngressEffectIntent::ArchiveAuthoritative { .. }))
            .expect("the real MAM copy has recorded archive authority");
        let receipt = receipt_key(archive).expect("archive receipt");
        EffectReceiptRepository::record_receipt(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("completion proof for the already committed archive copy");
        ArchiveDispatchRepository::record(
            &mut tx,
            key,
            &recipient.to_bare(),
            archive_ordinal.expect("archive position"),
            &[ArchiveDispatchObligation {
                receipt: obligation.receipt.clone(),
                target: DispatchTarget::Resource(recipient.clone()),
            }],
        )
        .await
        .expect("original frozen resource audience");
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
        route_progress: vec![progress.clone()],
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
        assert_eq!(
            rows[0].id,
            crate::ingress::ambiguous_offline_pending_id(key, &obligation.receipt),
            "the physical row is derived from the recorded host route obligation"
        );
        assert_eq!(
            fixture.count("ingress_effect_intents").await,
            2,
            "only RouteDirect and ArchiveAuthoritative authorize this handoff"
        );
        assert_eq!(
            fixture.count("ingress_archive_dispatch").await,
            if delay_recovery { 2 } else { 1 }
        );
        if delay_recovery {
            let mut tx = fixture
                .uow
                .begin()
                .await
                .expect("replay original archive registration");
            ArchiveDispatchRepository::record(
                &mut tx,
                key,
                &recipient.to_bare(),
                archive_ordinal.expect("position"),
                &[ArchiveDispatchObligation {
                    receipt: obligation.receipt.clone(),
                    target: DispatchTarget::Resource(recipient.clone()),
                }],
            )
            .await
            .expect("supplemental pending pin preserves original resource replay");
            tx.commit().await.expect("registration replay");
        }
    }
    if matches!(policy, RecipientPolicy::NoPermanentStore) {
        assert!(matches!(rows[0].payload, PendingPayload::Transient(_)));
    }
    if delay_recovery {
        assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            1
        );
        assert_sources_survive_gc(&fixture).await;
        assert_eq!(fixture.count("ingress_effect_intents").await, 2);
        assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    }
    if matches!(timing, RecoveryTiming::DelayedMissingBridge) {
        let PendingPayload::Archived(stamp) = &rows[0].payload else {
            panic!("archived copy")
        };
        let candidate = crate::notification_outbox::direct_candidate_from_envelope(
            &submission.plan.sanitized_message,
            &recipient.to_bare(),
            &submission.sender.clone().into(),
            stamp,
        )
        .expect("legacy frozen candidate");
        state
            .deps
            .protocol
            .notification_outbox
            .insert_candidate(&candidate)
            .await
            .expect("legacy raw scheduling row");
        fixture
            .execute("UPDATE notification_candidates SET outboxed_at_ms = 1", ())
            .await;
        assert_eq!(fixture.count("ingress_effect_descendants").await, 0);
        assert_eq!(fixture.count("notification_outbox_lineage").await, 0);
    }
    let sweep = reconcile_xep0357_notification_candidates_for_sweep(
        &state,
        64,
        &mut crate::server::routes::interpret::PendingNotificationRecoveryCursor::default(),
    )
    .await;
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
    if matches!(policy, RecipientPolicy::Notify) {
        let conn = fixture.db.guard().await.expect("custody read");
        let mut descendants = conn
            .query(
                "SELECT CAST(message_key AS TEXT), kind, semantic_identity_hash FROM ingress_effect_descendants WHERE settled_at IS NULL",
                (),
            )
            .await
            .expect("route descendant");
        let descendant = descendants
            .next()
            .await
            .expect("descendant row")
            .expect("route retains recovered notification");
        assert_eq!(
            descendant.get::<String>(0).expect("key"),
            key.to_storage().to_string()
        );
        assert_eq!(
            descendant.get::<i64>(1).expect("kind"),
            i64::from(obligation.receipt.kind.to_storage())
        );
        assert_eq!(
            descendant.get::<Vec<u8>>(2).expect("hash"),
            obligation.receipt.semantic_identity_hash.to_vec()
        );
        assert!(descendants.next().await.expect("only descendant").is_none());
    }
    assert!(pending
        .list_unoutboxed_archived(64)
        .await
        .expect("outboxed pending")
        .is_empty());
    if matches!(timing, RecoveryTiming::DelayedMissingBridge) {
        assert_eq!(
            pending
                .delete_row(&rows[0].id)
                .await
                .expect("consume legacy duplicate pending row"),
            1
        );
        assert_sources_survive_gc(&fixture).await;
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            1
        );
        assert_eq!(
            fixture
                .count("ingress_effect_descendants WHERE settled_at IS NULL")
                .await,
            1
        );
        assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
        fixture.close().await;
        return;
    }
    if delay_recovery {
        add_late_parent_to_marked_pending(
            &fixture,
            &state,
            &deps,
            &submission,
            &progress,
            &rows[0],
            matches!(timing, RecoveryTiming::DelayedUnknownFanout),
        )
        .await;
        if matches!(timing, RecoveryTiming::DelayedUnknownFanout) {
            fixture.close().await;
            return;
        }
    }
    for row in rows {
        assert_eq!(
            pending
                .delete_row(&row.id)
                .await
                .expect("consume pending delivery"),
            1
        );
    }
    if delay_recovery {
        assert_sources_survive_gc(&fixture).await;
        assert_eq!(fixture.count("ingress_messages").await, 2);
        assert_eq!(
            fixture
                .count("ingress_effect_descendants WHERE settled_at IS NULL")
                .await,
            2
        );
        assert_eq!(fixture.count("ingress_effect_intents").await, 4);
        assert_eq!(fixture.count("ingress_effect_receipts").await, 4);
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
    let sweep = reconcile_xep0357_notification_candidates_for_sweep(
        &state,
        64,
        &mut crate::server::routes::interpret::PendingNotificationRecoveryCursor::default(),
    )
    .await;
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

async fn add_late_parent_to_marked_pending(
    fixture: &IngressFixture,
    state: &Arc<crate::server::routes::websocket::WebSocketState>,
    deps: &Deps<'_>,
    original: &crate::ingress::IngressSubmission,
    progress: &RouteProgress,
    physical: &PendingRow,
    unknown_history: bool,
) {
    let recipient = original
        .plan
        .intents
        .iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::RouteDirect { fanout, .. } => fanout.first(),
            _ => None,
        })
        .expect("original route records the fixture recipient");
    let mut submission = fixture.submission(None, "recover the push after the sender pod dies");
    submission.plan.intents = original.plan.intents.clone();
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("late canonical parent");
    let key = decision.message_key.expect("late key");
    let obligation = SendObligation {
        message: key,
        receipt: progress.receipt.clone(),
        recipient: recipient.clone(),
    };
    assert_ne!(pending_id(key, &progress.receipt), physical.id);
    let mut tx = fixture.uow.begin().await.expect("late send reservation");
    let SendClaim::Acquired(lease) = SendAttemptRepository::claim(
        &mut tx,
        &obligation,
        &NodeIdentity::local(),
        Duration::from_secs(60),
    )
    .await
    .expect("late claim") else {
        panic!("fresh late lease")
    };
    assert!(SendAttemptRepository::start(&mut tx, &lease)
        .await
        .expect("late start"));
    let archive = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::ArchiveAuthoritative { .. }))
        .expect("existing archive authority");
    let receipt = receipt_key(archive).expect("archive identity");
    EffectReceiptRepository::record_receipt(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("same real committed archive copy");
    tx.commit().await.expect("late crashed send");
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;

    // A legacy outboxed candidate with no recorded fanout still holds its
    // pending bridge. PubSub/API acceptance cannot settle that unknown history.
    fixture
        .execute("UPDATE notification_candidates SET outboxed_at_ms = 1", ())
        .await;
    assert!(handoff(&fixture.uow, deps, key, progress, recipient)
        .await
        .expect("unknown fanout defers")
        .is_none());
    assert_eq!(fixture.count("ingress_archive_dispatch").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    if unknown_history {
        assert_eq!(
            deps.pending_delivery_storage
                .expect("strict pending storage")
                .delete_row(&physical.id)
                .await
                .expect("consume marked unknown row"),
            1
        );
        assert_sources_survive_gc(fixture).await;
        assert_eq!(fixture.count("ingress_messages").await, 2);
        let mut tx = fixture.uow.begin().await.expect("deferred send authority");
        assert_eq!(
            SendAttemptRepository::status(&mut tx, &obligation)
                .await
                .expect("unretired attempt"),
            Some(SendAttemptStatus::ExpiredStarted)
        );
        tx.commit().await.expect("authority read");
        // Once the consumed physical row is gone, the original unresolved
        // route can acquire a new unmarked row. Periodic recovery must preserve
        // the existing unknown bridge even when this parent is fully terminal.
        assert!(handoff(&fixture.uow, deps, key, progress, recipient)
            .await
            .expect("new physical custody")
            .is_some());
        let mut tx = fixture.uow.begin().await.expect("terminal late parent");
        assert!(CanonicalMessageRepository::lock(&mut tx, key)
            .await
            .expect("canonical lock"));
        CanonicalMessageRepository::terminalize(&mut tx, key, chrono::Utc::now())
            .await
            .expect("real archived and pending handoff proofs");
        tx.commit().await.expect("terminal late commit");
        let sweep = reconcile_xep0357_notification_candidates_for_sweep(
            state,
            64,
            &mut crate::server::routes::interpret::PendingNotificationRecoveryCursor::default(),
        )
        .await;
        assert!(!sweep.had_failure);
        assert_eq!(sweep.completed, 1);
        assert_eq!(fixture.count("notification_candidates").await, 1);
        assert_eq!(fixture.count("notification_outbox_lineage").await, 0);
        assert_eq!(
            fixture
                .count("ingress_effect_descendants WHERE settled_at IS NULL")
                .await,
            2
        );
        assert_eq!(fixture.count("ingress_effect_receipts").await, 4);
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            2
        );
        assert_eq!(
            deps.pending_delivery_storage
                .expect("pending storage")
                .delete_row(&pending_id(key, &progress.receipt))
                .await
                .expect("consume newly recovered row"),
            1
        );
        assert_sources_survive_gc(fixture).await;
        return;
    }
    fixture
        .execute(
            "UPDATE notification_candidates SET outboxed_at_ms = NULL",
            (),
        )
        .await;

    fixture
        .execute(
            "UPDATE notification_candidates SET quarantined_at_ms = 1",
            (),
        )
        .await;
    assert!(handoff(&fixture.uow, deps, key, progress, recipient)
        .await
        .expect("ambiguous candidate defers")
        .is_none());
    assert_eq!(
        fixture.count("ingress_archive_dispatch").await,
        2,
        "deferred handoff rolls back its supplemental pin"
    );
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        3,
        "the original route remains unreceipted"
    );
    fixture
        .execute(
            "UPDATE notification_candidates SET quarantined_at_ms = NULL",
            (),
        )
        .await;

    let postgres = fixture.db.driver() == crate::db::DatabaseDriver::Postgres;
    if postgres {
        fixture.execute("CREATE FUNCTION fail_late_handoff() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected late lineage failure'; END $$", ()).await;
        fixture.execute("CREATE TRIGGER fail_late_handoff BEFORE INSERT ON ingress_effect_descendants FOR EACH ROW EXECUTE FUNCTION fail_late_handoff()", ()).await;
    } else {
        fixture.execute("CREATE TRIGGER fail_late_handoff BEFORE INSERT ON ingress_effect_descendants BEGIN SELECT RAISE(ABORT, 'injected late lineage failure'); END", ()).await;
    }
    assert!(handoff(&fixture.uow, deps, key, progress, recipient)
        .await
        .is_err());
    assert_eq!(
        fixture.count("ingress_archive_dispatch").await,
        2,
        "candidate-transfer failure rolls back the physical custody pin"
    );
    assert_eq!(fixture.count("ingress_effect_descendants").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("failed handoff preserves send authority");
    assert_eq!(
        SendAttemptRepository::status(&mut tx, &obligation)
            .await
            .expect("live original attempt"),
        Some(SendAttemptStatus::ExpiredStarted)
    );
    tx.commit().await.expect("rollback verification");
    fixture
        .execute(
            if postgres {
                "DROP TRIGGER fail_late_handoff ON ingress_effect_descendants"
            } else {
                "DROP TRIGGER fail_late_handoff"
            },
            (),
        )
        .await;
    if postgres {
        fixture
            .execute("DROP FUNCTION fail_late_handoff()", ())
            .await;
    }

    assert!(handoff(&fixture.uow, deps, key, progress, recipient)
        .await
        .expect("validated late handoff")
        .is_some());
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("terminal completed late execution");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("late canonical lock"));
    CanonicalMessageRepository::terminalize(&mut tx, key, chrono::Utc::now())
        .await
        .expect("both existing obligations have real completion receipts");
    tx.commit().await.expect("late terminal proof");
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        2
    );
    assert_eq!(
        fixture.count("pending_delivery").await,
        1,
        "coalescing retains the actual original physical row"
    );
    assert_eq!(fixture.count("ingress_archive_dispatch").await, 3);
    assert_eq!(
        fixture
            .count("ingress_archive_dispatch WHERE pending_row_id <> ''")
            .await,
        2
    );
    assert_eq!(fixture.count("notification_candidates").await, 1);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        2
    );
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        4,
        "no new notification intent or completion receipt is fabricated"
    );
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
async fn sqlite_ambiguous_handoff_survives_gc_before_delayed_notification_recovery() {
    recovered_attempt_notification_with_delay(
        IngressFixture::sqlite().await,
        RecipientPolicy::Notify,
        CrashPhase::Started,
        RecoveryTiming::Delayed,
    )
    .await;
}

#[tokio::test]
async fn postgres_ambiguous_handoff_survives_gc_before_delayed_notification_recovery() {
    if let Some(fixture) = IngressFixture::postgres("delayed_ambiguous_push").await {
        recovered_attempt_notification_with_delay(
            fixture,
            RecipientPolicy::Notify,
            CrashPhase::Started,
            RecoveryTiming::Delayed,
        )
        .await;
    }
}

#[tokio::test]
async fn sqlite_marked_ambiguous_handoff_preserves_unknown_fanout_after_consumption() {
    recovered_attempt_notification_with_delay(
        IngressFixture::sqlite().await,
        RecipientPolicy::Notify,
        CrashPhase::Started,
        RecoveryTiming::DelayedUnknownFanout,
    )
    .await;
}

#[tokio::test]
async fn postgres_marked_ambiguous_handoff_preserves_unknown_fanout_after_consumption() {
    if let Some(fixture) = IngressFixture::postgres("unknown_ambiguous_push").await {
        recovered_attempt_notification_with_delay(
            fixture,
            RecipientPolicy::Notify,
            CrashPhase::Started,
            RecoveryTiming::DelayedUnknownFanout,
        )
        .await;
    }
}

#[tokio::test]
async fn sqlite_periodic_duplicate_retains_legacy_candidate_without_prior_bridge() {
    recovered_attempt_notification_with_delay(
        IngressFixture::sqlite().await,
        RecipientPolicy::Notify,
        CrashPhase::Started,
        RecoveryTiming::DelayedMissingBridge,
    )
    .await;
}

#[tokio::test]
async fn postgres_periodic_duplicate_retains_legacy_candidate_without_prior_bridge() {
    if let Some(fixture) = IngressFixture::postgres("missing_ambiguous_bridge").await {
        recovered_attempt_notification_with_delay(
            fixture,
            RecipientPolicy::Notify,
            CrashPhase::Started,
            RecoveryTiming::DelayedMissingBridge,
        )
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

async fn assert_sources_survive_gc(fixture: &IngressFixture) {
    for days in [9, 18] {
        let gc = crate::ingress_substrate::gc_expired_aliases(
            &fixture.db,
            chrono::Utc::now() + chrono::Duration::days(days),
            crate::ingress_substrate::AliasGcBudget {
                deadline: tokio::time::Instant::now() + Duration::from_secs(5),
                lock_timeout: Duration::from_millis(100),
                statement_timeout: Duration::from_secs(2),
                scan_timeout: Duration::from_secs(2),
                progress: Default::default(),
            },
        )
        .await
        .expect("retained pending or notification custody across delayed GC");
        assert_eq!(gc.deleted_messages, 0);
    }
}
