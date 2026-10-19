use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_substrate::{AliasGcBudget, AliasGcProgress, MessageEnvelope, ALIAS_RETENTION};
use crate::ingress_uow::{
    CanonicalMessageRepository, EffectIntentRepository, EffectReceiptRepository,
};
use crate::notification_outbox::drain::{enqueue_outbox_job_tx, mark_candidate_outboxed_tx};
use crate::notification_outbox::test_support::{candidate, target_named};
use waddle_xmpp::ingress::{NotificationCandidateOutcome, SemanticDigest};
use xmpp_parsers::message::Message;

fn budget() -> AliasGcBudget {
    AliasGcBudget {
        deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        lock_timeout: std::time::Duration::from_secs(1),
        statement_timeout: std::time::Duration::from_secs(2),
        scan_timeout: std::time::Duration::from_secs(2),
        progress: AliasGcProgress::default(),
    }
}

async fn legacy_parent(fixture: &IngressFixture, candidate: &NotificationCandidate) -> MessageKey {
    legacy_parent_for_conversation(fixture, candidate, &candidate.conversation_jid).await
}

async fn legacy_parent_for_conversation(
    fixture: &IngressFixture,
    candidate: &NotificationCandidate,
    recorded_conversation: &BareJid,
) -> MessageKey {
    let key = MessageKey::new();
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: candidate.recipient_bare_jid.clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: recorded_conversation.clone(),
            archive_stanza_id: candidate.archive_stanza_id.clone(),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let mut message = Message::new(Some(Jid::from(candidate.recipient_bare_jid.clone())));
    message.from = Some(candidate.sender_jid.clone());
    message.bodies.insert(
        xmpp_parsers::message::Lang::from(""),
        "original approved message".to_string(),
    );
    let mut tx = fixture.uow.begin().await.expect("legacy canonical intent");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [1; 32]).expect("digest"),
        Some(&MessageEnvelope::new(message)),
    )
    .await
    .expect("message");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("intent");
    let hash: [u8; 32] = Sha256::digest(intent.semantic_key().storage_identity().as_bytes()).into();
    EffectReceiptRepository::record_receipt(
        &mut tx,
        key,
        crate::ingress_substrate::EffectReceiptKind::from_storage(7),
        &hash,
    )
    .await
    .expect("old receipt");
    CanonicalMessageRepository::terminalize(
        &mut tx,
        key,
        chrono::Utc::now() - chrono::Duration::days(20),
    )
    .await
    .expect("old terminal clock");
    tx.commit().await.expect("commit legacy parent");
    key
}

async fn stable_legacy_id(
    store: &NotificationOutboxStore,
    candidate: &NotificationCandidate,
) -> Uuid {
    let mut tx = store
        .db
        .begin_immediate()
        .await
        .expect("startup identity adoption");
    let id = NotificationOutboxStore::candidate_delivery_id_in_transaction(&mut tx, candidate)
        .await
        .expect("id")
        .expect("candidate exists");
    tx.commit().await.expect("identity commit");
    id
}

async fn pending_candidate_receipt_gap(fixture: IngressFixture) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let candidate = candidate("legacy-candidate-gap");
    let key = legacy_parent(&fixture, &candidate).await;
    store
        .insert_candidate(&candidate)
        .await
        .expect("legacy candidate");
    store
        .execute(
            "UPDATE notification_candidates SET delivery_id = NULL WHERE stanza_id = ?",
            crate::db_params![candidate.archive_stanza_id.id.clone()],
        )
        .await
        .expect("pre-key schema row");
    let id = stable_legacy_id(&store, &candidate).await;
    store
        .adopt_legacy_ancestry()
        .await
        .expect("adopt canonical receipt gap");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert_eq!(
        crate::ingress_substrate::gc_expired_aliases(&fixture.db, chrono::Utc::now(), budget())
            .await
            .expect("GC after adoption")
            .deleted_messages,
        0
    );
    assert_eq!(
        stable_legacy_id(&store, &candidate).await,
        id,
        "startup retains the same candidate identity"
    );
    store.adopt_legacy_ancestry().await.expect("repeat startup");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1,
        "repeat adoption adds no duplicate custody"
    );
    let mut tx = fixture.uow.begin().await.expect("retained authority");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("canonical row retained"));
    tx.commit().await.expect("read commit");
    fixture.close().await;
}

async fn pending_provider_and_restart_reconcile(fixture: IngressFixture) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let provider = crate::push_service::DatabasePushServiceStore::new(fixture.db.clone())
        .await
        .expect("provider schema");
    let candidate = candidate("legacy-published-gap");
    legacy_parent(&fixture, &candidate).await;
    store
        .insert_candidate(&candidate)
        .await
        .expect("legacy candidate");
    let node = provider
        .ensure_node(&candidate.recipient_bare_jid, "legacy-app")
        .await
        .expect("provider node");
    let target = NotificationOutboxTarget::new(
        "push.example.com".parse().expect("service"),
        PushServiceNodeName::new(node.node()).expect("node"),
    );
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("old coalesced job");
    enqueue_outbox_job_tx(
        &mut tx,
        &candidate,
        &target,
        &build_waddle_context(&candidate),
        &RichSummary::minimal(),
        crate::time::now_ms(),
    )
    .await
    .expect("job");
    mark_candidate_outboxed_tx(&mut tx, &candidate, crate::time::now_ms())
        .await
        .expect("outboxed");
    tx.execute("DELETE FROM notification_outbox_lineage", ())
        .await
        .expect("old schema had no lineage");
    tx.execute("UPDATE notification_candidates SET delivery_id = NULL", ())
        .await
        .expect("old candidate had no key");
    tx.commit().await.expect("old job commit");
    let job = store.pending_outbox_jobs().await.expect("job").remove(0);
    let original = job.context().clone();
    store
        .execute(
            "UPDATE notification_outbox SET status = 'published' WHERE job_id = ?",
            crate::db_params![job.job_id().as_str()],
        )
        .await
        .expect("old PubSub ACK");
    fixture.execute("INSERT INTO push_publish_jobs(job_id,owner_bare_jid,push_service_jid,node,item_id,payload_xml,acceptance_scope,status,created_at_ms,updated_at_ms) VALUES(?,?,?,?,?,?,'legacy','queued',?,?)",crate::db_params![Uuid::new_v4().to_string(),candidate.recipient_bare_jid.to_string(),target.push_service_jid().to_string(),node.node(),job.job_id().as_str(),String::from(&job.to_xep0357_pubsub_item().payload.expect("approved payload")),crate::time::now_ms(),crate::time::now_ms()]).await;
    let id = stable_legacy_id(&store, &candidate).await;
    store
        .adopt_legacy_ancestry()
        .await
        .expect("published is not provider acceptance");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1,
        "only provider job retains the parent after candidate handoff"
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        1
    );
    assert_eq!(
        crate::ingress_substrate::gc_expired_aliases(&fixture.db, chrono::Utc::now(), budget())
            .await
            .expect("provider custody GC")
            .deleted_messages,
        0
    );
    // A later same-conversation job must not become an ancestor of an already
    // adopted candidate when startup repeats.
    let unrelated = crate::notification_outbox::test_support::candidate("later-unrelated-message");
    let mut tx = fixture.db.begin_immediate().await.expect("later job");
    enqueue_outbox_job_tx(
        &mut tx,
        &unrelated,
        &target_named("later-node"),
        &build_waddle_context(&unrelated),
        &RichSummary::minimal(),
        crate::time::now_ms(),
    )
    .await
    .expect("later job");
    tx.commit().await.expect("later commit");
    store
        .adopt_legacy_ancestry()
        .await
        .expect("repeat while provider pending");
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        1,
        "repeat startup follows recorded links, not new jobs"
    );
    assert_eq!(stable_legacy_id(&store, &candidate).await, id);
    fixture
        .execute(
            "UPDATE push_publish_jobs SET status = 'published' WHERE item_id = ?",
            crate::db_params![job.job_id().as_str()],
        )
        .await;
    fixture
        .execute(
            "UPDATE push_publish_jobs SET acceptance_scope = 'wire' WHERE item_id = ?",
            crate::db_params![job.job_id().as_str()],
        )
        .await;
    store
        .adopt_legacy_ancestry()
        .await
        .expect("wire completion is not Foundation acceptance");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1,
        "a public wire publication cannot discharge host ancestry"
    );
    fixture
        .execute(
            "UPDATE push_publish_jobs SET acceptance_scope = 'legacy' WHERE item_id = ?",
            crate::db_params![job.job_id().as_str()],
        )
        .await;
    let before = chrono::Utc::now();
    store
        .adopt_legacy_ancestry()
        .await
        .expect("reconcile explicit provider completion");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        0
    );
    assert_eq!(
        crate::ingress_substrate::gc_expired_aliases(
            &fixture.db,
            before + ALIAS_RETENTION - chrono::Duration::seconds(1),
            budget()
        )
        .await
        .expect("full post-provider tail")
        .deleted_messages,
        0
    );
    assert_eq!(
        crate::ingress_substrate::gc_expired_aliases(
            &fixture.db,
            chrono::Utc::now() + ALIAS_RETENTION + chrono::Duration::seconds(1),
            budget()
        )
        .await
        .expect("tail elapsed")
        .deleted_messages,
        1
    );
    let mut rows = store
        .query(
            "SELECT context_xml FROM notification_outbox WHERE job_id = ?",
            crate::db_params![job.job_id().as_str()],
        )
        .await
        .expect("original payload");
    let preserved: Element = rows
        .next()
        .await
        .expect("read")
        .expect("job remains")
        .get::<String>(0)
        .expect("stored context")
        .parse()
        .expect("typed context");
    assert_eq!(
        preserved, original,
        "ancestry adoption never enriches or mutates approved payload"
    );
    drop(rows);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_legacy_candidate_receipt_gap_preserves_evidence() {
    pending_candidate_receipt_gap(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_legacy_candidate_receipt_gap_preserves_evidence() {
    if let Some(fixture) = IngressFixture::postgres("legacy_candidate_ancestry").await {
        pending_candidate_receipt_gap(fixture).await;
    }
}
#[tokio::test]
async fn sqlite_legacy_provider_pending_and_repeat_startup_are_honest() {
    pending_provider_and_restart_reconcile(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_legacy_provider_pending_and_repeat_startup_are_honest() {
    if let Some(fixture) = IngressFixture::postgres("legacy_provider_ancestry").await {
        pending_provider_and_restart_reconcile(fixture).await;
    }
}

async fn malformed_legacy_candidate_boot_isolated_and_retained(fixture: IngressFixture) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("initial outbox");
    let bad_sender = candidate("malformed-sender");
    let bad_identity = candidate("malformed-identity");
    let bad_audit = candidate("malformed-audit");
    let good = candidate("valid-neighbor");
    let sender_key = legacy_parent(&fixture, &bad_sender).await;
    for item in [&bad_identity, &bad_audit, &good] {
        legacy_parent(&fixture, item).await;
    }
    for item in [&bad_sender, &bad_identity, &bad_audit, &good] {
        store.insert_candidate(item).await.expect("candidate");
    }
    store.execute("UPDATE notification_candidates SET sender_jid = ?, outboxed_at_ms = 1 WHERE stanza_id = ?", crate::db_params!["invalid sender", bad_sender.archive_stanza_id.id.clone()]).await.expect("malformed sender");
    store.execute("UPDATE notification_candidates SET delivery_id = ?, outboxed_at_ms = 1 WHERE stanza_id = ?", crate::db_params!["invalid-delivery-id", bad_identity.archive_stanza_id.id.clone()]).await.expect("malformed identity");
    let mut conn = fixture
        .db
        .begin_immediate()
        .await
        .expect("corruption fixture");
    if fixture.db.driver() == DatabaseDriver::Sqlite {
        conn.execute("PRAGMA ignore_check_constraints = ON", ())
            .await
            .expect("legacy unchecked fixture");
    } else {
        conn.execute("ALTER TABLE notification_candidates DROP CONSTRAINT notification_candidates_suppressed_reason_check", ()).await.expect("legacy audit constraint");
    }
    conn.execute("UPDATE notification_candidates SET suppressed_reason = ?, outboxed_at_ms = 1 WHERE stanza_id = ?", crate::db_params!["unknown-old-audit", bad_audit.archive_stanza_id.id.clone()]).await.expect("malformed audit");
    if fixture.db.driver() == DatabaseDriver::Sqlite {
        conn.execute("PRAGMA ignore_check_constraints = OFF", ())
            .await
            .expect("restore checks");
    } else {
        let values = SuppressedReason::ALL
            .iter()
            .map(|reason| format!("'{}'", reason.as_db_value()))
            .collect::<Vec<_>>()
            .join(",");
        conn.execute(&format!("ALTER TABLE notification_candidates ADD CONSTRAINT notification_candidates_suppressed_reason_check CHECK (suppressed_reason IN ({values})) NOT VALID"), ()).await.expect("retain legacy invalid row with current audit shape");
    }
    conn.commit().await.expect("persist corruption fixture");
    let reopened = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("malformed scheduler rows must not brick startup");
    assert_eq!(
        fixture
            .count("notification_candidates WHERE quarantined_at_ms IS NOT NULL")
            .await,
        3
    );
    assert_eq!(fixture.count("notification_candidates WHERE quarantined_at_ms IS NOT NULL AND outboxed_at_ms = 1").await, 3, "original history timestamps survive");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        4,
        "quarantine and valid-neighbor custody remains pending"
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NOT NULL")
            .await,
        0
    );
    let pending = reopened
        .pending_candidates(16)
        .await
        .expect("valid scheduler neighbor");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].archive_stanza_id(), good.archive_stanza_id());
    let future = chrono::Utc::now() + ALIAS_RETENTION + chrono::Duration::days(1);
    assert_eq!(
        crate::ingress_substrate::gc_expired_aliases(&fixture.db, future, budget())
            .await
            .expect("future GC")
            .deleted_messages,
        0
    );
    assert_eq!(
        reopened
            .prune_completed_before(future.timestamp_millis(), 64)
            .await
            .expect("future scheduler prune")
            .candidates_deleted,
        0
    );
    assert_eq!(fixture.count("notification_candidates").await, 4);
    let mut audit = reopened.query(
        "SELECT suppressed_reason, quarantined_suppressed_reason FROM notification_candidates WHERE stanza_id = ?",
        crate::db_params![bad_audit.archive_stanza_id.id.clone()],
    ).await.expect("preserved audit");
    let row = audit.next().await.expect("row").expect("audit row");
    assert_eq!(row.get::<Option<String>>(0).expect("active audit"), None);
    assert_eq!(
        row.get::<String>(1).expect("original audit"),
        "unknown-old-audit"
    );
    drop(audit);
    // Repairing payload fields is not a durable disposition of unknown history.
    reopened
        .execute(
            "UPDATE notification_candidates SET sender_jid = ? WHERE stanza_id = ?",
            crate::db_params![
                bad_sender.sender_jid.to_string(),
                bad_sender.archive_stanza_id.id.clone()
            ],
        )
        .await
        .expect("manual sender repair");
    reopened
        .execute(
            "UPDATE notification_candidates SET suppressed_reason = ? WHERE stanza_id = ?",
            crate::db_params![
                SuppressedReason::Xep0357NoRegistration.as_db_value(),
                bad_audit.archive_stanza_id.id.clone()
            ],
        )
        .await
        .expect("manual audit repair");
    NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("repeat boot after manual repair");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        4,
        "repeat boot reuses quarantine identities without releasing history"
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NOT NULL")
            .await,
        0
    );
    let mut tx = fixture.uow.begin().await.expect("stored intent");
    let intent = EffectIntentRepository::load(&mut tx, sender_key)
        .await
        .expect("load")
        .remove(0);
    tx.commit().await.expect("release read");
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("canonical replay");
    EffectDescendantRepository::lock_raw(&mut tx, sender_key)
        .await
        .expect("canonical lock");
    assert!(
        matches!(
            NotificationOutboxStore::attach_candidate_lineage_in_transaction(
                &mut tx,
                sender_key,
                &intent.semantic_key(),
                &bad_sender
            )
            .await,
            Err(IngressUowError::EffectIntentConflict)
        ),
        "canonical replay cannot adopt or settle quarantined work"
    );
    drop(tx);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        4
    );
    let mut rows = reopened
        .query(
            "SELECT delivery_id FROM notification_candidates WHERE stanza_id = ?",
            crate::db_params![bad_identity.archive_stanza_id.id.clone()],
        )
        .await
        .expect("identity evidence");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("quarantined row")
            .get::<String>(0)
            .expect("original identity"),
        "invalid-delivery-id"
    );
    drop(rows);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_malformed_legacy_candidate_boot_isolated_and_retained() {
    malformed_legacy_candidate_boot_isolated_and_retained(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_malformed_legacy_candidate_boot_isolated_and_retained() {
    if let Some(fixture) = IngressFixture::postgres("malformed_notification").await {
        malformed_legacy_candidate_boot_isolated_and_retained(fixture).await;
    }
}

async fn scanned_parent_collected_before_adoption(fixture: IngressFixture) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    for (label, malformed) in [("scanned-good", false), ("scanned-malformed", true)] {
        let item = candidate(label);
        legacy_parent(&fixture, &item).await;
        store.insert_candidate(&item).await.expect("candidate");
        if malformed {
            store
                .execute(
                    "UPDATE notification_candidates SET sender_jid = ? WHERE stanza_id = ?",
                    crate::db_params!["invalid sender", item.archive_stanza_id.id.clone()],
                )
                .await
                .expect("malformed sender");
        }
    }
    let now = chrono::Utc::now();
    let recorded = load_page(&fixture.db, None).await.expect("startup scan");
    assert_eq!(recorded.len(), 2);
    let collected = crate::ingress_substrate::gc_expired_aliases(&fixture.db, now, budget())
        .await
        .expect("concurrent collector");
    assert_eq!(collected.deleted_messages, 2);
    for item in &recorded {
        store
            .adopt_recorded(item, AdoptionPhase::Ancestry)
            .await
            .expect("already-collected scan is benign");
    }
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    assert_eq!(
        fixture.count("ingress_effect_descendants").await,
        0,
        "never recreate collected authority"
    );
    NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("boot after collection");
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_scanned_parent_collected_before_adoption_is_benign() {
    scanned_parent_collected_before_adoption(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_scanned_parent_collected_before_adoption_is_benign() {
    if let Some(fixture) = IngressFixture::postgres("adoption_gc_race").await {
        scanned_parent_collected_before_adoption(fixture).await;
    }
}

async fn malformed_audit_before_stale_check_rebuild(fixture: IngressFixture) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let mut item = candidate("stale-audit-check");
    item.last_message_body = Some("frozen snapshot".to_owned());
    legacy_parent(&fixture, &item).await;
    store
        .insert_candidate(&item)
        .await
        .expect("legacy candidate");
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("legacy schema fixture");
    if fixture.db.driver() == DatabaseDriver::Sqlite {
        let mut rows = tx.query("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'notification_candidates'", ()).await.expect("table shape");
        let sql: String = rows
            .next()
            .await
            .expect("row")
            .expect("table")
            .get(0)
            .expect("SQL");
        drop(rows);
        let stale = sql
            .replace(
                super::super::schema::NOTIFICATION_CANDIDATES_CLASS_CHECK_SQL,
                "class IN ('dm')",
            )
            .replace(
                super::super::schema::NOTIFICATION_CANDIDATES_REASON_CHECK_SQL,
                "reason IN ('offline_dm')",
            );
        assert_ne!(stale, sql);
        tx.execute(
            "ALTER TABLE notification_candidates RENAME TO notification_candidates_audit_seed",
            (),
        )
        .await
        .expect("legacy rename");
        tx.execute(&stale, ()).await.expect("stale checks");
        tx.execute(
            "INSERT INTO notification_candidates SELECT * FROM notification_candidates_audit_seed",
            (),
        )
        .await
        .expect("legacy row");
        tx.execute("DROP TABLE notification_candidates_audit_seed", ())
            .await
            .expect("drop fixture source");
        tx.execute("PRAGMA ignore_check_constraints = ON", ())
            .await
            .expect("unchecked audit fixture");
    } else {
        tx.execute("ALTER TABLE notification_candidates DROP CONSTRAINT notification_candidates_suppressed_reason_check", ()).await.expect("old audit check");
        let values = SuppressedReason::ALL
            .iter()
            .map(|reason| format!("'{}'", reason.as_db_value()))
            .chain(std::iter::once("'unknown-old-audit'".to_owned()))
            .collect::<Vec<_>>()
            .join(",");
        tx.execute(&format!("ALTER TABLE notification_candidates ADD CONSTRAINT notification_candidates_suppressed_reason_check CHECK (suppressed_reason IN ({values}))"), ()).await.expect("stale superset");
    }
    tx.execute(
        "UPDATE notification_candidates SET suppressed_reason = ? WHERE stanza_id = ?",
        crate::db_params!["unknown-old-audit", item.archive_stanza_id.id.clone()],
    )
    .await
    .expect("old audit");
    if fixture.db.driver() == DatabaseDriver::Sqlite {
        tx.execute("PRAGMA ignore_check_constraints = OFF", ())
            .await
            .expect("restore checks");
    }
    tx.commit().await.expect("legacy fixture commit");
    let reopened = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("quarantine before all CHECK migrations");
    let mut rows = reopened.query("SELECT suppressed_reason,quarantined_suppressed_reason,quarantined_at_ms,last_message_body,delivery_id FROM notification_candidates WHERE stanza_id = ?", crate::db_params![item.archive_stanza_id.id.clone()]).await.expect("preserved evidence");
    let row = rows.next().await.expect("row").expect("candidate");
    assert_eq!(row.get::<Option<String>>(0).expect("active audit"), None);
    assert_eq!(
        row.get::<String>(1).expect("original audit"),
        "unknown-old-audit"
    );
    assert!(row
        .get::<Option<i64>>(2)
        .expect("quarantine marker")
        .is_some());
    assert_eq!(
        row.get::<String>(3).expect("frozen body"),
        "frozen snapshot"
    );
    assert!(row
        .get::<Option<String>>(4)
        .expect("delivery identity")
        .is_some());
    drop(rows);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert!(reopened
        .pending_candidates(16)
        .await
        .expect("worker selection")
        .is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_malformed_audit_survives_all_stale_check_rebuilds() {
    malformed_audit_before_stale_check_rebuild(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_malformed_audit_survives_stale_check_rebuild() {
    if let Some(fixture) = IngressFixture::postgres("quarantine_stale_check").await {
        malformed_audit_before_stale_check_rebuild(fixture).await;
    }
}

#[tokio::test]
async fn postgres_canonical_candidate_lock_fences_other_parent_quarantine() {
    let Some(fixture) = IngressFixture::postgres("candidate_quarantine_lock").await else {
        return;
    };
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let item = candidate("shared-quarantine-target");
    let first = legacy_parent(&fixture, &item).await;
    let second = legacy_parent(&fixture, &item).await;
    store.insert_candidate(&item).await.expect("candidate");
    store
        .execute(
            "UPDATE notification_candidates SET sender_jid = ? WHERE stanza_id = ?",
            crate::db_params!["invalid sender", item.archive_stanza_id.id.clone()],
        )
        .await
        .expect("malformed snapshot");
    let mut rows = store.query("SELECT recipient_bare_jid,conversation_jid,sender_jid,thread_id,stanza_id_by,stanza_id,class,reason,policy_error_count,noping,no_store,no_permanent_store,last_message_body,reaction,delivery_id,outboxed_at_ms,suppressed_reason,quarantined_at_ms FROM notification_candidates WHERE stanza_id = ?", crate::db_params![item.archive_stanza_id.id.clone()]).await.expect("startup snapshot");
    let snapshot = rows.next().await.expect("row").expect("candidate");
    drop(rows);
    store
        .execute(
            "UPDATE notification_candidates SET sender_jid = ? WHERE stanza_id = ?",
            crate::db_params![
                item.sender_jid.to_string(),
                item.archive_stanza_id.id.clone()
            ],
        )
        .await
        .expect("operator repair after startup scan");
    let recorded = load_page(&fixture.db, None)
        .await
        .expect("recorded authority");
    let first_recorded = recorded
        .iter()
        .find(|row| row.message == first)
        .expect("first parent");
    let second_recorded = recorded
        .iter()
        .find(|row| row.message == second)
        .expect("second parent");
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("canonical acceptance");
    NotificationOutboxStore::attach_candidate_lineage_in_transaction(
        &mut tx,
        first,
        &first_recorded.intent.semantic_key(),
        &item,
    )
    .await
    .expect("canonical candidate custody");
    assert!(
        matches!(
            store
                .quarantine_recorded_candidate(second_recorded, &snapshot)
                .await,
            Err(IngressUowError::Database {
                retry_class: crate::ingress_uow::DbRetryClass::CanonicalLockContention
            })
        ),
        "quarantine cannot interleave with candidate acceptance by another parent"
    );
    tx.commit().await.expect("acceptance commit");
    assert_eq!(
        fixture
            .count("notification_candidates WHERE quarantined_at_ms IS NOT NULL")
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1,
        "failed quarantine leaves no partial reference"
    );
    store
        .quarantine_recorded_candidate(second_recorded, &snapshot)
        .await
        .expect("quarantine after acceptance releases lock");
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("duplicate replay");
    assert!(matches!(
        NotificationOutboxStore::attach_candidate_lineage_in_transaction(
            &mut tx,
            first,
            &first_recorded.intent.semantic_key(),
            &item
        )
        .await,
        Err(IngressUowError::EffectIntentConflict)
    ));
    drop(tx);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        2
    );
    fixture.close().await;
}

async fn ambiguous_legacy_jobs_preserve_candidate(fixture: IngressFixture, mixed: bool) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let provider = crate::push_service::DatabasePushServiceStore::new(fixture.db.clone())
        .await
        .expect("provider");
    let wanted = candidate("legacy-missing-real-job");
    let unrelated = candidate("legacy-other-message");
    legacy_parent(&fixture, &wanted).await;
    store
        .insert_candidate(&wanted)
        .await
        .expect("wanted candidate");
    for (item, label) in [
        (&unrelated, "ambiguous-target"),
        (&wanted, "matched-target"),
    ] {
        if label == "matched-target" && !mixed {
            continue;
        }
        let node = provider
            .ensure_node(&wanted.recipient_bare_jid, label)
            .await
            .expect("node");
        let target = NotificationOutboxTarget::new(
            "push.example.com".parse().expect("service"),
            PushServiceNodeName::new(node.node()).expect("node"),
        );
        let mut tx = fixture.db.begin_immediate().await.expect("legacy job");
        enqueue_outbox_job_tx(
            &mut tx,
            item,
            &target,
            &build_waddle_context(item),
            &RichSummary::minimal(),
            crate::time::now_ms(),
        )
        .await
        .expect("job");
        tx.commit().await.expect("job commit");
    }
    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    for job in &jobs {
        let completed = !mixed || job.context() == &build_waddle_context(&wanted);
        if completed {
            store
                .execute(
                    "UPDATE notification_outbox SET status = 'published' WHERE job_id = ?",
                    crate::db_params![job.job_id().as_str()],
                )
                .await
                .expect("old PubSub acceptance");
        }
        let payload = job.to_xep0357_pubsub_item().payload.expect("payload");
        fixture.execute("INSERT INTO push_publish_jobs(job_id,owner_bare_jid,push_service_jid,node,item_id,payload_xml,acceptance_scope,status,created_at_ms,updated_at_ms) VALUES(?,?,?,?,?,?,'legacy',?,?,?)",
            crate::db_params![Uuid::new_v4().to_string(), wanted.recipient_bare_jid.to_string(), job.push_service_jid().to_string(), job.node().as_str(), job.job_id().as_str(), String::from(&payload), if completed { "published" } else { "queued" }, crate::time::now_ms(), crate::time::now_ms()]).await;
    }
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("pre-lineage history");
    mark_candidate_outboxed_tx(&mut tx, &wanted, crate::time::now_ms())
        .await
        .expect("old handoff");
    tx.execute("DELETE FROM notification_outbox_lineage", ())
        .await
        .expect("old schema had no links");
    tx.commit().await.expect("legacy commit");
    let id = stable_legacy_id(&store, &wanted).await;
    let pruned_early = mixed && fixture.db.driver() == DatabaseDriver::Sqlite;
    if pruned_early {
        let matched = jobs
            .iter()
            .find(|job| job.context() == &build_waddle_context(&wanted))
            .expect("matched job");
        store.execute(&format!("CREATE TRIGGER fail_legacy_child_adoption BEFORE INSERT ON ingress_effect_descendants WHEN NEW.descendant_key = '{}' BEGIN SELECT RAISE(ABORT, 'injected legacy child adoption failure'); END", matched.job_id().as_str()), ()).await.expect("adoption fault injection");
        store
            .adopt_legacy_ancestry()
            .await
            .expect_err("interruption after scanning ambiguous fanout");
        store
            .execute("DROP TRIGGER fail_legacy_child_adoption", ())
            .await
            .expect("restore adoption");
        store.execute("UPDATE notification_outbox SET status = 'published', updated_at_ms = 1 WHERE job_id <> ?", crate::db_params![matched.job_id().as_str()]).await.expect("ambiguous job becomes prunable");
        assert_eq!(
            store
                .prune_completed_before(2, 64)
                .await
                .expect("prune before restart")
                .jobs_deleted,
            1
        );
    }
    for _ in 0..2 {
        store
            .adopt_legacy_ancestry()
            .await
            .expect("conservative adoption");
        let mut rows = store.query("SELECT count(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![id.to_string()]).await.expect("candidate custody");
        assert_eq!(rows.next().await.expect("row").expect("count").get::<i64>(0).expect("pending count"), 1,
            "unrelated completion or ambiguous coalesced target cannot discharge the candidate bridge");
        drop(rows);
        assert_eq!(
            fixture.count("notification_outbox_lineage").await,
            i64::from(mixed),
            "only the reconstructable matched job gains lineage"
        );
    }
    store
        .execute(
            "UPDATE notification_outbox SET status = 'published', updated_at_ms = 1",
            (),
        )
        .await
        .expect("old jobs become prunable");
    assert_eq!(
        store
            .prune_completed_before(crate::time::now_ms(), 64)
            .await
            .expect("prune unknown legacy job")
            .jobs_deleted,
        u64::from(!pruned_early)
    );
    store
        .adopt_legacy_ancestry()
        .await
        .expect("restart after ambiguous job was pruned");
    let mut rows = store.query("SELECT count(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![id.to_string()]).await.expect("durable candidate uncertainty");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("pending count"),
        1,
        "pruning cannot erase previously observed unknown fanout"
    );
    drop(rows);
    assert_eq!(
        crate::ingress_substrate::gc_expired_aliases(
            &fixture.db,
            chrono::Utc::now() + ALIAS_RETENTION + chrono::Duration::days(1),
            budget()
        )
        .await
        .expect("future GC")
        .deleted_messages,
        0
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_unrelated_legacy_completion_never_discharges_candidate() {
    ambiguous_legacy_jobs_preserve_candidate(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn postgres_unrelated_legacy_completion_never_discharges_candidate() {
    if let Some(fixture) = IngressFixture::postgres("unrelated_legacy_job").await {
        ambiguous_legacy_jobs_preserve_candidate(fixture, false).await;
    }
}

#[tokio::test]
async fn sqlite_mixed_legacy_fanout_keeps_ambiguous_custody_pending() {
    ambiguous_legacy_jobs_preserve_candidate(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn postgres_mixed_legacy_fanout_keeps_ambiguous_custody_pending() {
    if let Some(fixture) = IngressFixture::postgres("mixed_legacy_jobs").await {
        ambiguous_legacy_jobs_preserve_candidate(fixture, true).await;
    }
}

async fn legacy_archive_aliases_share_candidate_custody(
    fixture: IngressFixture,
    stored_alias: bool,
) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let item = candidate("cross-archive-adoption");
    let first = legacy_parent(&fixture, &item).await;
    let mut alias = item.clone();
    alias.archive_stanza_id.by = "legacy-archive.example.com".parse().expect("archive alias");
    let second = legacy_parent_for_conversation(&fixture, &alias, &alias.recipient_bare_jid).await;
    let mut foreign = alias.clone();
    foreign.sender_jid = "bob@example.com/other-source"
        .parse()
        .expect("foreign source");
    let rejected =
        legacy_parent_for_conversation(&fixture, &foreign, &foreign.recipient_bare_jid).await;
    store
        .insert_candidate(&item)
        .await
        .expect("shared cross-archive candidate");
    let mut wrong_class = item.clone();
    wrong_class.class = NotificationClass::DirectMessageMention;
    wrong_class.reason = NotificationReason::OfflineDirectMessageMention;
    store
        .insert_candidate(&wrong_class)
        .await
        .expect("mismatched legacy class");
    let mut wrong_thread = item.clone();
    wrong_thread.thread_id = NotificationThreadId::new("wrong-thread");
    store
        .insert_candidate(&wrong_thread)
        .await
        .expect("mismatched legacy thread");
    if stored_alias {
        store
            .execute(
                "UPDATE notification_candidates SET stanza_id_by = ? WHERE stanza_id = ?",
                crate::db_params![
                    alias.archive_stanza_id.by.to_string(),
                    item.archive_stanza_id.id.clone()
                ],
            )
            .await
            .expect("stored legacy archive authority");
    }
    store
        .adopt_legacy_ancestry()
        .await
        .expect("adopt both archive authorities");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        2,
        "both canonical archive aliases must retain the shared unresolved candidate"
    );
    let id = stable_legacy_id(&store, &item).await;
    let mut rows = store
        .query(
            "SELECT stanza_id_by, delivery_id FROM notification_candidates WHERE stanza_id = ? AND class = 'dm' AND thread_id = ''",
            crate::db_params![item.archive_stanza_id.id.clone()],
        )
        .await
        .expect("stored identity");
    let row = rows.next().await.expect("row").expect("candidate");
    assert_eq!(
        row.get::<String>(0).expect("stored authority"),
        if stored_alias {
            alias.archive_stanza_id.by.to_string()
        } else {
            item.archive_stanza_id.by.to_string()
        }
    );
    assert_eq!(
        row.get::<String>(1).expect("stable identity"),
        id.to_string()
    );
    drop(rows);
    store.adopt_legacy_ancestry().await.expect("repeat startup");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        2
    );
    assert_eq!(
        crate::ingress_substrate::gc_expired_aliases(&fixture.db, chrono::Utc::now(), budget())
            .await
            .expect("archive alias GC")
            .deleted_messages,
        1,
        "only the mismatched-source parent is collectible"
    );
    let mut tx = fixture.uow.begin().await.expect("retained parents");
    assert!(CanonicalMessageRepository::lock(&mut tx, first)
        .await
        .expect("first"));
    assert!(CanonicalMessageRepository::lock(&mut tx, second)
        .await
        .expect("second"));
    assert!(!CanonicalMessageRepository::lock(&mut tx, rejected)
        .await
        .expect("rejected source"));
    tx.commit().await.expect("read commit");
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_legacy_archive_aliases_retain_shared_candidate_custody() {
    legacy_archive_aliases_share_candidate_custody(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn postgres_legacy_archive_aliases_retain_shared_candidate_custody() {
    if let Some(f) = IngressFixture::postgres("legacy_cross_archive").await {
        legacy_archive_aliases_share_candidate_custody(f, false).await;
    }
}

#[tokio::test]
async fn sqlite_legacy_stored_archive_alias_preserves_intrinsic_validation() {
    legacy_archive_aliases_share_candidate_custody(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn postgres_legacy_stored_archive_alias_preserves_intrinsic_validation() {
    if let Some(f) = IngressFixture::postgres("legacy_stored_alias").await {
        legacy_archive_aliases_share_candidate_custody(f, true).await;
    }
}

async fn malformed_cross_archive_candidate_retains_exact_row_and_parents(fixture: IngressFixture) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let item = candidate("malformed-cross-archive")
        .with_last_message_body(Some("quarantine evidence".into()));
    legacy_parent(&fixture, &item).await;
    let mut alias = item.clone();
    alias.archive_stanza_id.by = "legacy-archive.example.com".parse().expect("alias");
    legacy_parent_for_conversation(&fixture, &alias, &alias.recipient_bare_jid).await;
    store
        .insert_candidate(&item)
        .await
        .expect("legacy candidate");
    store.execute("UPDATE notification_candidates SET sender_jid = ?, stanza_id_by = ? WHERE stanza_id = ?", crate::db_params!["invalid sender", alias.archive_stanza_id.by.to_string(), item.archive_stanza_id.id.clone()]).await.expect("malformed stored alias");
    let reopened = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("quarantine cross-archive preflight");
    assert_eq!(
        fixture
            .count("notification_candidates WHERE quarantined_at_ms IS NOT NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        2
    );
    let mut rows = reopened.query("SELECT stanza_id_by, sender_jid, last_message_body FROM notification_candidates WHERE stanza_id = ?", crate::db_params![item.archive_stanza_id.id.clone()]).await.expect("original row");
    let row = rows.next().await.expect("row").expect("candidate");
    assert_eq!(
        row.get::<String>(0).expect("by"),
        alias.archive_stanza_id.by.to_string()
    );
    assert_eq!(row.get::<String>(1).expect("sender"), "invalid sender");
    assert_eq!(row.get::<String>(2).expect("body"), "quarantine evidence");
    drop(rows);
    reopened
        .adopt_legacy_ancestry()
        .await
        .expect("repeat startup");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        2
    );
    assert_eq!(
        crate::ingress_substrate::gc_expired_aliases(&fixture.db, chrono::Utc::now(), budget())
            .await
            .expect("quarantine GC")
            .deleted_messages,
        0
    );
    drop(reopened);
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_malformed_cross_archive_alias_keeps_exact_quarantine_identity() {
    malformed_cross_archive_candidate_retains_exact_row_and_parents(IngressFixture::sqlite().await)
        .await;
}
#[tokio::test]
async fn postgres_malformed_cross_archive_alias_keeps_exact_quarantine_identity() {
    if let Some(f) = IngressFixture::postgres("malformed_cross_archive").await {
        malformed_cross_archive_candidate_retains_exact_row_and_parents(f).await;
    }
}
