use super::*;
use crate::ingress::{commit::commit_submission, receipt_key, test_support::IngressFixture};
use crate::ingress_substrate::{gc_expired_aliases, AliasGcBudget};
use crate::ingress_uow::{
    ArchiveDispatchObligation, ArchiveDispatchRepository, CanonicalMessageRepository,
    DispatchTarget, EffectReceiptRepository, PendingReceiptRepository,
};
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey, PendingDeliveryMutation};
use waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal;
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId, QuotaPolicy};

struct PendingRecoveryFixture {
    state: Arc<WebSocketState>,
    row: PendingRow,
    key: MessageKey,
    ordinal: waddle_xmpp::mam::ArchiveOrdinal,
}

async fn pending_recovery_fixture(fixture: &IngressFixture) -> PendingRecoveryFixture {
    pending_recovery_fixture_with_row_id(fixture, PendingRowId::fresh()).await
}

async fn pending_recovery_fixture_with_row_id(
    fixture: &IngressFixture,
    row_id: PendingRowId,
) -> PendingRecoveryFixture {
    let state =
        crate::server::routes::websocket::tests::create_test_websocket_state_with_durable_ingress(
            fixture,
        )
        .await;
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "recovery")
        .await
        .expect("node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "device",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("registration");
    let stamp =
        waddle_xmpp_core::xep0359::StanzaId::new("pending-recovery", recipient.clone().into());
    let row = PendingRow {
        id: row_id,
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Archived(stamp.clone()),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    insert_pending_recovery(fixture, state, row, "frozen canonical body").await
}

async fn insert_pending_recovery(
    fixture: &IngressFixture,
    state: Arc<WebSocketState>,
    row: PendingRow,
    body: &str,
) -> PendingRecoveryFixture {
    let recipient = row.recipient.clone();
    let PendingPayload::Archived(stamp) = &row.payload else {
        panic!("archived fixture")
    };
    let stamp = stamp.clone();
    let pending = IngressEffectIntent::PendingDelivery {
        mutation: PendingDeliveryMutation::Archived {
            recipient: recipient.clone(),
            row_id: row.id.clone(),
            archive_stanza_id: stamp.clone(),
        },
    };
    let mut submission = fixture.submission(None, body);
    submission.plan.intents = vec![pending.clone()];
    let mut mutable_archive_message = submission.plan.sanitized_message.clone();
    mutable_archive_message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "mutable MAM substitute".to_string(),
    );
    let archived = waddle_xmpp_core::mam::ArchivedMessage {
        id: stamp.id.clone(),
        stanza_id: Some(stamp.clone()),
        body: Some("mutable MAM substitute".to_string()),
        stanza_xml: Some(String::from(&minidom::Element::from(
            mutable_archive_message,
        ))),
        message_type: xmpp_parsers::message::MessageType::Chat,
        ..waddle_xmpp_core::mam::ArchivedMessage::for_test(
            "romeo@example.com/phone".parse().expect("sender"),
            recipient.clone().into(),
        )
    };
    state
        .deps
        .protocol
        .mam_storage
        .store_message(&recipient, &archived)
        .await
        .expect("MAM copy");
    let stored = state
        .deps
        .protocol
        .mam_storage
        .get_message_by_archive_or_stanza_id(&recipient, stamp.as_str())
        .await
        .expect("archive read")
        .expect("archive copy");
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit canonical source");
    let key = decision.message_key.expect("canonical key");
    let receipt = receipt_key(&pending).expect("pending receipt");
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("source custody transaction");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("lock source"));
    ArchiveDispatchRepository::record(
        &mut tx,
        key,
        &recipient,
        stored.ordinal.expect("durable archive position"),
        &[ArchiveDispatchObligation {
            receipt: receipt.clone(),
            target: DispatchTarget::Pending(row.id.clone()),
        }],
    )
    .await
    .expect("exact pending provenance");
    PendingReceiptRepository::insert(&mut tx, &row, QuotaPolicy::Unlimited)
        .await
        .expect("pending custody");
    EffectReceiptRepository::record_receipt(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("pending insertion proof");
    CanonicalMessageRepository::terminalize(
        &mut tx,
        key,
        chrono::Utc::now() - chrono::Duration::days(9),
    )
    .await
    .expect("canonical execution complete");
    tx.commit().await.expect("commit pending source");
    PendingRecoveryFixture {
        state,
        row,
        key,
        ordinal: stored.ordinal.expect("archive position"),
    }
}

async fn recovered_pending_candidate_survives_consumption_and_gc(fixture: IngressFixture) {
    let PendingRecoveryFixture { state, row, .. } = pending_recovery_fixture(&fixture).await;

    let recovered = reconcile_xep0357_notification_candidates_for_sweep(
        &state,
        16,
        &mut PendingNotificationRecoveryCursor::default(),
    )
    .await;
    assert_eq!(recovered.completed, 1);
    assert!(!recovered.had_failure);
    assert_eq!(
        state
            .deps
            .protocol
            .notification_outbox
            .count_all_candidates()
            .await
            .expect("candidate"),
        1
    );
    assert_eq!(
        state
            .deps
            .protocol
            .pending_delivery_storage
            .delete_row(&row.id)
            .await
            .expect("recipient consumes pending copy"),
        1
    );
    // Once pending custody is consumed, maintenance first refreshes the full
    // retention tail, then a later pass must still retain the push descendant.
    for gc_time in [
        chrono::Utc::now() + chrono::Duration::days(9),
        chrono::Utc::now() + chrono::Duration::days(18),
    ] {
        let gc = gc_expired_aliases(
            &fixture.db,
            gc_time,
            AliasGcBudget {
                deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                lock_timeout: std::time::Duration::from_millis(100),
                statement_timeout: std::time::Duration::from_secs(2),
                scan_timeout: std::time::Duration::from_secs(2),
                progress: Default::default(),
            },
        )
        .await
        .expect("retention GC");
        assert_eq!(
            gc.deleted_messages, 0,
            "an unresolved recovered candidate retains canonical custody after pending consumption"
        );
    }
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("notification_candidates WHERE last_message_body = 'frozen canonical body'")
            .await,
        1,
        "recovery uses the admitted canonical envelope rather than mutable MAM content"
    );
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        1,
        "recovery adds custody without inventing a notification receipt"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_periodic_pending_recovery_retains_candidate_after_consumption() {
    recovered_pending_candidate_survives_consumption_and_gc(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_periodic_pending_recovery_retains_candidate_after_consumption() {
    if let Some(fixture) = IngressFixture::postgres("periodic_notification_custody").await {
        recovered_pending_candidate_survives_consumption_and_gc(fixture).await;
    }
}

async fn add_coalesced_parent(
    fixture: &IngressFixture,
    recovery: &PendingRecoveryFixture,
    body: &str,
) -> MessageKey {
    record_coalesced_parent(fixture, recovery, body, PendingRowId::fresh()).await
}

async fn record_coalesced_parent(
    fixture: &IngressFixture,
    recovery: &PendingRecoveryFixture,
    body: &str,
    host_row: PendingRowId,
) -> MessageKey {
    let PendingPayload::Archived(stamp) = &recovery.row.payload else {
        panic!("archived fixture")
    };
    let pending = IngressEffectIntent::PendingDelivery {
        mutation: PendingDeliveryMutation::Archived {
            recipient: recovery.row.recipient.clone(),
            row_id: host_row.clone(),
            archive_stanza_id: stamp.clone(),
        },
    };
    let mut submission = fixture.submission(None, body);
    submission.plan.intents = vec![pending.clone()];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("coalesced canonical parent");
    let key = decision.message_key.expect("key");
    let receipt = receipt_key(&pending).expect("receipt");
    let mut tx = fixture.uow.begin().await.expect("parent transaction");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("parent lock"));
    ArchiveDispatchRepository::record(
        &mut tx,
        key,
        &recovery.row.recipient,
        recovery.ordinal,
        &[ArchiveDispatchObligation {
            receipt: receipt.clone(),
            target: DispatchTarget::Pending(host_row.clone()),
        }],
    )
    .await
    .expect("coalesced copy provenance");
    EffectReceiptRepository::record_receipt(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("shared pending custody proof");
    CanonicalMessageRepository::terminalize(
        &mut tx,
        key,
        chrono::Utc::now() - chrono::Duration::days(9),
    )
    .await
    .expect("terminal parent");
    tx.commit().await.expect("parent commit");
    key
}

async fn collect_at(fixture: &IngressFixture, days: i64) -> usize {
    gc_expired_aliases(
        &fixture.db,
        chrono::Utc::now() + chrono::Duration::days(days),
        AliasGcBudget {
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(5),
            lock_timeout: std::time::Duration::from_millis(100),
            statement_timeout: std::time::Duration::from_secs(2),
            scan_timeout: std::time::Duration::from_secs(2),
            progress: Default::default(),
        },
    )
    .await
    .expect("retention GC")
    .deleted_messages
}

fn frozen_candidate(
    preparation: &crate::ingress_uow::PendingNotificationRecovery,
    row: &PendingRow,
) -> crate::notification_outbox::NotificationCandidate {
    let PendingPayload::Archived(stamp) = &row.payload else {
        panic!("archived fixture")
    };
    crate::notification_outbox::direct_candidate_from_envelope(
        preparation.envelope().message(),
        &row.recipient,
        preparation
            .envelope()
            .message()
            .from
            .as_ref()
            .expect("sender"),
        stamp,
    )
    .expect("frozen candidate")
}

async fn pending_recovery_mixed_parents_and_stale_source(fixture: IngressFixture) {
    let recovery = pending_recovery_fixture(&fixture).await;
    let preparation = recovery
        .state
        .deps
        .protocol
        .ingress
        .prepare_pending_notification_recovery(&recovery.row)
        .await
        .expect("prepare before parent change");
    let crate::ingress_uow::PendingNotificationPreparation::Ready(preparation) = preparation else {
        panic!("canonical source")
    };
    let second = add_coalesced_parent(&fixture, &recovery, "frozen canonical body").await;
    assert_ne!(second, recovery.key);
    // A coalesced parent with no physical pending ID can already be collected;
    // recovery must retain only surviving authority rather than resurrect it.
    assert_eq!(collect_at(&fixture, 9).await, 1);
    let candidate = frozen_candidate(&preparation, &recovery.row);
    let result = recovery
        .state
        .deps
        .protocol
        .ingress
        .settle_pending_notification_recovery(
            &preparation,
            crate::ingress::RecoveryPolicyDecision::Deliver(Box::new(candidate)),
        )
        .await
        .expect("settle surviving parent");
    assert_eq!(result, crate::ingress::RecoverySweepOutcome::Completed);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(fixture.count("ingress_messages").await, 1);
    fixture.close().await;
}

async fn pending_recovery_live_parents_and_ambiguity(fixture: IngressFixture, contradictory: bool) {
    let recovery = pending_recovery_fixture(&fixture).await;
    let preparation = recovery
        .state
        .deps
        .protocol
        .ingress
        .prepare_pending_notification_recovery(&recovery.row)
        .await
        .expect("prepare before new live parent");
    let crate::ingress_uow::PendingNotificationPreparation::Ready(preparation) = preparation else {
        panic!("source")
    };
    add_coalesced_parent(
        &fixture,
        &recovery,
        if contradictory {
            "contradictory frozen body"
        } else {
            "frozen canonical body"
        },
    )
    .await;
    if contradictory {
        let result = reconcile_xep0357_notification_candidates_for_sweep(
            &recovery.state,
            16,
            &mut PendingNotificationRecoveryCursor::default(),
        )
        .await;
        assert!(result.had_failure);
        assert_eq!(result.completed, 0);
        assert_eq!(fixture.count("notification_candidates").await, 0);
        assert_eq!(fixture.count("ingress_effect_descendants").await, 0);
        assert_eq!(
            fixture
                .count("pending_delivery WHERE notification_outboxed_at_ms IS NULL")
                .await,
            1
        );
    } else {
        let settled = recovery
            .state
            .deps
            .protocol
            .ingress
            .settle_pending_notification_recovery(
                &preparation,
                crate::ingress::RecoveryPolicyDecision::Deliver(Box::new(frozen_candidate(
                    &preparation,
                    &recovery.row,
                ))),
            )
            .await
            .expect("include the new live parent after preparation");
        assert_eq!(settled, crate::ingress::RecoverySweepOutcome::Completed);
        let repeated = reconcile_xep0357_notification_candidates_for_sweep(
            &recovery.state,
            16,
            &mut PendingNotificationRecoveryCursor::default(),
        )
        .await;
        assert_eq!(repeated.completed, 0);
        assert!(!repeated.had_failure);
        assert_eq!(fixture.count("notification_candidates").await, 1);
        assert_eq!(
            fixture
                .count("ingress_effect_descendants WHERE settled_at IS NULL")
                .await,
            2
        );
        recovery
            .state
            .deps
            .protocol
            .pending_delivery_storage
            .delete_row(&recovery.row.id)
            .await
            .expect("consume");
        assert_eq!(collect_at(&fixture, 9).await, 0);
        assert_eq!(collect_at(&fixture, 18).await, 0);
    }
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

async fn pending_recovery_marker_fault_rolls_back(fixture: IngressFixture) {
    let recovery = pending_recovery_fixture(&fixture).await;
    let postgres = fixture.db.driver() == crate::db::DatabaseDriver::Postgres;
    if postgres {
        fixture.execute("CREATE FUNCTION fail_pending_notification_marker() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'forced marker failure'; END $$", ()).await;
        fixture.execute("CREATE TRIGGER fail_pending_notification_marker BEFORE UPDATE OF notification_outboxed_at_ms ON pending_delivery FOR EACH ROW EXECUTE FUNCTION fail_pending_notification_marker()", ()).await;
    } else {
        fixture.execute("CREATE TRIGGER fail_pending_notification_marker BEFORE UPDATE OF notification_outboxed_at_ms ON pending_delivery BEGIN SELECT RAISE(ABORT, 'forced marker failure'); END", ()).await;
    }
    let failed = reconcile_xep0357_notification_candidates_for_sweep(
        &recovery.state,
        16,
        &mut PendingNotificationRecoveryCursor::default(),
    )
    .await;
    assert!(failed.had_failure);
    assert_eq!(failed.completed, 0);
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert_eq!(fixture.count("ingress_effect_descendants").await, 0);
    assert_eq!(
        fixture
            .count("pending_delivery WHERE notification_outboxed_at_ms IS NULL")
            .await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture
        .execute(
            if postgres {
                "DROP TRIGGER fail_pending_notification_marker ON pending_delivery"
            } else {
                "DROP TRIGGER fail_pending_notification_marker"
            },
            (),
        )
        .await;
    let retried = reconcile_xep0357_notification_candidates_for_sweep(
        &recovery.state,
        16,
        &mut PendingNotificationRecoveryCursor::default(),
    )
    .await;
    assert!(!retried.had_failure);
    assert_eq!(retried.completed, 1);
    assert_eq!(fixture.count("notification_candidates").await, 1);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

async fn pending_recovery_collected_preparation_is_not_recreated(fixture: IngressFixture) {
    let recovery = pending_recovery_fixture(&fixture).await;
    let preparation = recovery
        .state
        .deps
        .protocol
        .ingress
        .prepare_pending_notification_recovery(&recovery.row)
        .await
        .expect("prepare");
    let crate::ingress_uow::PendingNotificationPreparation::Ready(preparation) = preparation else {
        panic!("source")
    };
    recovery
        .state
        .deps
        .protocol
        .pending_delivery_storage
        .delete_row(&recovery.row.id)
        .await
        .expect("consume before completion");
    assert_eq!(collect_at(&fixture, 9).await, 0);
    assert_eq!(collect_at(&fixture, 18).await, 1);
    let result = recovery
        .state
        .deps
        .protocol
        .ingress
        .settle_pending_notification_recovery(
            &preparation,
            crate::ingress::RecoveryPolicyDecision::Deliver(Box::new(frozen_candidate(
                &preparation,
                &recovery.row,
            ))),
        )
        .await
        .expect("collected source handled");
    assert_eq!(result, crate::ingress::RecoverySweepOutcome::CanonicalGone);
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert_eq!(fixture.count("ingress_effect_descendants").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_pending_notification_recovery_contracts() {
    pending_recovery_live_parents_and_ambiguity(IngressFixture::sqlite().await, false).await;
    pending_recovery_live_parents_and_ambiguity(IngressFixture::sqlite().await, true).await;
    pending_recovery_mixed_parents_and_stale_source(IngressFixture::sqlite().await).await;
    pending_recovery_marker_fault_rolls_back(IngressFixture::sqlite().await).await;
    pending_recovery_collected_preparation_is_not_recreated(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_pending_notification_recovery_contracts() {
    for (name, case) in [
        ("pending_live_parents", 0),
        ("pending_conflicting_parents", 1),
        ("pending_collected_parent", 2),
        ("pending_marker_fault", 3),
        ("pending_stale_preparation", 4),
    ] {
        if let Some(fixture) = IngressFixture::postgres(name).await {
            match case {
                0 => pending_recovery_live_parents_and_ambiguity(fixture, false).await,
                1 => pending_recovery_live_parents_and_ambiguity(fixture, true).await,
                2 => pending_recovery_mixed_parents_and_stale_source(fixture).await,
                3 => pending_recovery_marker_fault_rolls_back(fixture).await,
                _ => pending_recovery_collected_preparation_is_not_recreated(fixture).await,
            }
        }
    }
}

async fn notification_marker(fixture: &IngressFixture, id: &PendingRowId) -> bool {
    let mut rows = fixture
        .db
        .guard()
        .await
        .expect("database guard")
        .query(
            "SELECT notification_outboxed_at_ms FROM pending_delivery WHERE row_id = ?",
            crate::db_params![id.as_str()],
        )
        .await
        .expect("marker query");
    rows.next()
        .await
        .expect("marker row")
        .expect("pending row retained")
        .get::<Option<i64>>(0)
        .expect("marker")
        .is_some()
}

async fn insert_fair_recovery(
    fixture: &IngressFixture,
    recovery: &PendingRecoveryFixture,
    id: PendingRowId,
    label: &str,
) -> PendingRecoveryFixture {
    let mut row = recovery.row.clone();
    row.id = id;
    row.payload = PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
        label,
        row.recipient.clone().into(),
    ));
    insert_pending_recovery(fixture, recovery.state.clone(), row, label).await
}

async fn pending_notification_janitor_pages_past_contradictions(fixture: IngressFixture) {
    let mut ids = (0..4).map(|_| PendingRowId::fresh()).collect::<Vec<_>>();
    ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    let sentinel = pending_recovery_fixture_with_row_id(
        &fixture,
        PendingRowId::new(hex::encode([255_u8; 32])),
    )
    .await;
    let completed = reconcile_xep0357_notification_candidates_for_sweep(
        &sentinel.state,
        2,
        &mut PendingNotificationRecoveryCursor::default(),
    )
    .await;
    assert_eq!(completed.completed, 1);
    assert!(notification_marker(&fixture, &sentinel.row.id).await);
    let recovery = insert_fair_recovery(
        &fixture,
        &sentinel,
        ids[1].clone(),
        "fair-contradictory-first",
    )
    .await;
    let contradictory = record_coalesced_parent(
        &fixture,
        &recovery,
        "contradictory frozen body",
        recovery.row.id.clone(),
    )
    .await;
    assert_ne!(recovery.key, contradictory);
    let mut second_row = recovery.row.clone();
    second_row.id = ids[2].clone();
    second_row.payload = PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
        "fair-contradictory-second",
        second_row.recipient.clone().into(),
    ));
    let second = insert_pending_recovery(
        &fixture,
        recovery.state.clone(),
        second_row,
        "frozen canonical body",
    )
    .await;
    let second_contradictory = record_coalesced_parent(
        &fixture,
        &second,
        "another contradictory frozen body",
        second.row.id.clone(),
    )
    .await;
    let valid = insert_fair_recovery(&fixture, &recovery, ids[3].clone(), "fair-valid").await;
    let storage = recovery
        .state
        .deps
        .protocol
        .pending_delivery_storage
        .as_ref();
    assert_eq!(
        storage
            .notification_recovery_high_water()
            .await
            .expect("ceiling")
            .expect("physical rows")
            .to_storage(),
        4,
        "four physical inserts, despite the marked sentinel's greater row ID"
    );
    assert!(storage
        .list_unoutboxed_archived_after(
            None,
            Some(PendingNotificationRecoveryOrdinal::from_storage(4).expect("ceiling")),
            0
        )
        .await
        .expect("zero page")
        .is_empty());
    let page = storage
        .list_unoutboxed_archived_after(
            Some(PendingNotificationRecoveryOrdinal::from_storage(3).expect("after")),
            Some(PendingNotificationRecoveryOrdinal::from_storage(4).expect("ceiling")),
            1,
        )
        .await
        .expect("strict bounded page");
    assert_eq!(
        page.iter().map(|row| &row.id).collect::<Vec<_>>(),
        vec![&valid.row.id]
    );
    let mut cursor = PendingNotificationRecoveryCursor::default();
    // These are actual janitor sweeps, including candidate expansion, delivery
    // and retention. Two contradictory physical rows fill the entire page.
    crate::server::session_janitors::run_notification_outbox_sweep(
        &recovery.state,
        2,
        8,
        2,
        &mut cursor,
    )
    .await;
    assert!(!notification_marker(&fixture, &recovery.row.id).await);
    assert!(!notification_marker(&fixture, &ids[2]).await);
    assert!(!notification_marker(&fixture, &valid.row.id).await);

    let mut suffix = Vec::new();
    for label in ["fair-tail-1", "fair-tail-2"] {
        let row = insert_fair_recovery(&fixture, &recovery, PendingRowId::fresh(), label).await;
        assert!(row.row.id.as_str() > valid.row.id.as_str());
        suffix.push(row.row.id);
    }
    crate::server::session_janitors::run_notification_outbox_sweep(
        &recovery.state,
        2,
        8,
        2,
        &mut cursor,
    )
    .await;
    assert!(
        notification_marker(&fixture, &valid.row.id).await,
        "a contradictory first page must not starve the later valid row"
    );
    for id in &suffix {
        assert!(
            !notification_marker(&fixture, id).await,
            "new suffix rows wait outside the current lap's high-water horizon"
        );
    }

    let behind = insert_fair_recovery(&fixture, &recovery, ids[0].clone(), "fair-behind").await;
    crate::server::session_janitors::run_notification_outbox_sweep(
        &recovery.state,
        2,
        8,
        2,
        &mut cursor,
    )
    .await;
    let mut newer = Vec::new();
    for tick in 0..2 {
        for index in 0..2 {
            let row = insert_fair_recovery(
                &fixture,
                &recovery,
                PendingRowId::fresh(),
                &format!("continuous-tail-{tick}-{index}"),
            )
            .await;
            newer.push(row.row.id);
        }
        crate::server::session_janitors::run_notification_outbox_sweep(
            &recovery.state,
            2,
            8,
            2,
            &mut cursor,
        )
        .await;
    }
    for id in &suffix {
        assert!(notification_marker(&fixture, id).await);
    }
    assert!(
        notification_marker(&fixture, &behind.row.id).await,
        "a lexically behind insertion must be recovered within the next finite lap"
    );
    for id in &newer {
        assert!(
            !notification_marker(&fixture, id).await,
            "continuous arrivals cannot extend a finite recovery lap"
        );
    }
    assert!(!notification_marker(&fixture, &recovery.row.id).await);
    assert!(!notification_marker(&fixture, &ids[2]).await);
    let mut rows = fixture
        .db
        .guard()
        .await
        .expect("database guard")
        .query(
            "SELECT COUNT(*) FROM ingress_effect_descendants WHERE CAST(message_key AS TEXT) IN (?, ?, ?, ?)",
            crate::db_params![
                recovery.key.to_storage().to_string(),
                contradictory.to_storage().to_string(),
                second.key.to_storage().to_string(),
                second_contradictory.to_storage().to_string()
            ],
        )
        .await
        .expect("contradictory parent custody");
    assert_eq!(
        rows.next()
            .await
            .expect("count row")
            .expect("count")
            .get::<i64>(0)
            .expect("descendant count"),
        0,
        "paging must not grant notification authority to contradictory parents"
    );
    assert_eq!(fixture.count("pending_delivery").await, 11);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_pending_notification_janitor_fair_finite_laps() {
    pending_notification_janitor_pages_past_contradictions(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_pending_notification_janitor_fair_finite_laps() {
    if let Some(fixture) = IngressFixture::postgres("pendingfair").await {
        pending_notification_janitor_pages_past_contradictions(fixture).await;
    }
}
