use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_substrate::{
    gc_expired_aliases, AliasGcBudget, MessageEnvelope, ALIAS_RETENTION,
};
use crate::ingress_uow::{
    CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
    EffectReceiptRepository, SmIngressRepository, SmIngressStreamRepository,
};
use crate::notification_outbox::test_support::{candidate, enqueue_jobs_for_test, target};
use waddle_xmpp::ingress::{
    IngressEffectIntent, IngressOrdinal, MessageKey, NotificationActivityMutation,
    NotificationCandidateOutcome, SemanticDigest, WireHandledCount,
};
use waddle_xmpp::pending_delivery::SmSessionId;

async fn parent(
    fixture: &IngressFixture,
    candidate: &NotificationCandidate,
) -> (MessageKey, IngressEffectIntent) {
    let key = MessageKey::new();
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: candidate.recipient_bare_jid.clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: candidate.conversation_jid.clone(),
            archive_stanza_id: candidate.archive_stanza_id.clone(),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let mut message =
        xmpp_parsers::message::Message::new(Some(candidate.recipient_bare_jid.clone().into()));
    message.from = Some(candidate.sender_jid.clone());
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "original approved message".to_string(),
    );
    let mut tx = fixture.uow.begin().await.expect("canonical parent");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [91; 32]).expect("digest"),
        Some(&MessageEnvelope::new(message)),
    )
    .await
    .expect("message");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("intent");
    let receipt = crate::ingress::receipt_key(&intent).expect("receipt");
    EffectReceiptRepository::record_receipt(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("operational receipt");
    CanonicalMessageRepository::terminalize(
        &mut tx,
        key,
        chrono::Utc::now() - chrono::Duration::days(20),
    )
    .await
    .expect("old terminal proof");
    tx.commit().await.expect("parent commit");
    (key, intent)
}

async fn backend_pid(tx: &mut crate::db::Transaction<'_>) -> i32 {
    let mut rows = tx
        .query("SELECT pg_backend_pid()", ())
        .await
        .expect("backend PID");
    rows.next()
        .await
        .expect("PID row")
        .expect("PID")
        .get(0)
        .expect("decode PID")
}

async fn blocked_pid(fixture: &IngressFixture, blocker: i32) -> Option<i32> {
    let conn = fixture.db.guard().await.expect("observe locks");
    let mut rows = conn.query("SELECT pid FROM pg_stat_activity WHERE ? = ANY(pg_blocking_pids(pid)) AND wait_event_type = 'Lock'", crate::db_params![blocker]).await.expect("blocked backend");
    rows.next()
        .await
        .expect("blocked row")
        .map(|row| row.get(0).expect("blocked PID"))
}

async fn await_prune_skip_or_lock(
    fixture: &IngressFixture,
    pruning: &tokio::task::JoinHandle<
        Result<NotificationOutboxPruneOutcome, NotificationOutboxError>,
    >,
    blocker: i32,
) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if pruning.is_finished() || blocked_pid(fixture, blocker).await.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("prune must promptly skip or reach the held row");
}

async fn candidate_prune_preserves_late_attachment_after_snapshot(
    fixture: IngressFixture,
    repeatable_read: bool,
) {
    let db = if repeatable_read {
        let mut url = url::Url::parse(fixture.db.database_url()).expect("fixture URL");
        let options = url
            .query_pairs()
            .find(|(key, _)| key == "options")
            .map(|(_, value)| value.into_owned())
            .unwrap_or_default();
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained)
            .append_pair(
                "options",
                &format!("{options} -c default_transaction_isolation=repeatable\\ read"),
            );
        let config =
            crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, url.to_string());
        Database::from_config("prune-repeatable-read", &config)
            .await
            .expect("fixture-local isolation pool")
    } else {
        fixture.db.clone()
    };
    if repeatable_read {
        let conn = db.guard().await.expect("session default");
        let mut rows = conn
            .query("SHOW default_transaction_isolation", ())
            .await
            .expect("configured isolation");
        let configured: String = rows
            .next()
            .await
            .expect("isolation row")
            .expect("row")
            .get(0)
            .expect("isolation value");
        assert_eq!(configured, "repeatable read");
    }
    let store = NotificationOutboxStore::new(db.clone())
        .await
        .expect("outbox");
    if repeatable_read {
        let mut tx = store
            .begin_prune_transaction()
            .await
            .expect("bounded prune transaction");
        let mut rows = tx
            .query("SHOW transaction_isolation", ())
            .await
            .expect("prune isolation");
        let isolation: String = rows
            .next()
            .await
            .expect("isolation row")
            .expect("row")
            .get(0)
            .expect("isolation value");
        assert_eq!(
            isolation, "read committed",
            "pruning must pin fresh guard snapshots before its first probe"
        );
        drop(rows);
        tx.commit().await.expect("isolation proof commit");
    }
    let candidate = candidate("late-prune-parent");
    store.insert_candidate(&candidate).await.expect("candidate");
    store
        .execute("UPDATE notification_candidates SET outboxed_at_ms = 1", ())
        .await
        .expect("old outboxed candidate");
    let (key, intent) = parent(&fixture, &candidate).await;
    let mut attachment = fixture.db.begin().await.expect("late attachment");
    let pid = backend_pid(&mut attachment).await;
    // The actual attachment path holds the candidate but does not rewrite its
    // dedup tuple. Its new settled reference is invisible to an older DELETE.
    NotificationOutboxStore::attach_candidate_lineage_in_transaction(
        &mut attachment,
        key,
        &intent.semantic_key(),
        &candidate,
    )
    .await
    .expect("attach late canonical parent");
    let pruning = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .prune_completed_before(crate::time::now_ms(), 16)
                .await
        }
    });
    await_prune_skip_or_lock(&fixture, &pruning, pid).await;
    attachment.commit().await.expect("late attachment commit");
    let outcome = pruning.await.expect("pruner").expect("prune result");
    assert_eq!(
        outcome.candidates_deleted, 0,
        "a stale DELETE snapshot must not erase newly attached dedup custody"
    );
    assert_eq!(
        store.count_all_candidates().await.expect("candidate count"),
        1
    );
    assert_eq!(fixture.count("ingress_effect_descendants").await, 1);
    drop(store);
    drop(db);
    fixture.close().await;
}

#[tokio::test]
async fn postgres_candidate_prune_preserves_late_attachment_after_snapshot() {
    if let Some(fixture) = IngressFixture::postgres("candidate_prune_snapshot").await {
        candidate_prune_preserves_late_attachment_after_snapshot(fixture, false).await;
    }
}

#[tokio::test]
async fn postgres_candidate_prune_pins_read_committed_over_repeatable_read_default() {
    if let Some(fixture) = IngressFixture::postgres("candidate_prune_repeatable_read").await {
        candidate_prune_preserves_late_attachment_after_snapshot(fixture, true).await;
    }
}

#[tokio::test]
async fn postgres_job_prune_preserves_late_legacy_attachment_after_snapshot() {
    let Some(fixture) = IngressFixture::postgres("job_prune_snapshot").await else {
        return;
    };
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let candidate = candidate("late-prune-job-parent");
    enqueue_jobs_for_test(&store, &candidate, &[target()]).await;
    let job = store.pending_outbox_jobs().await.expect("job").remove(0);
    store
        .execute(
            "UPDATE notification_outbox SET status = 'published', updated_at_ms = 1",
            (),
        )
        .await
        .expect("old terminal job");
    store
        .execute("UPDATE notification_candidates SET outboxed_at_ms = 1", ())
        .await
        .expect("old candidate");
    store
        .execute("DELETE FROM notification_outbox_lineage", ())
        .await
        .expect("model legacy scheduling gap");
    parent(&fixture, &candidate).await;
    // Pause the actual legacy adopter after its job lock, before the first
    // descendant INSERT. The fixture-only trigger leaves production untouched.
    store.execute("CREATE FUNCTION prune_attachment_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(7193931); RETURN NEW; END $$", ()).await.expect("gate function");
    store.execute(&format!("CREATE TRIGGER prune_attachment_gate BEFORE INSERT ON ingress_effect_descendants FOR EACH ROW WHEN (NEW.descendant_key = '{}') EXECUTE FUNCTION prune_attachment_gate()", job.job_id().as_str()), ()).await.expect("job attachment gate");
    let mut gate = fixture.db.begin().await.expect("hold gate");
    let gate_pid = backend_pid(&mut gate).await;
    let mut rows = gate
        .query("SELECT pg_advisory_xact_lock(7193931)", ())
        .await
        .expect("hold advisory gate");
    rows.next().await.expect("advisory row");
    drop(rows);
    let adoption = tokio::spawn({
        let store = store.clone();
        async move { store.adopt_legacy_ancestry().await }
    });
    let adopter_pid = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(pid) = blocked_pid(&fixture, gate_pid).await {
                break pid;
            }
            assert!(
                !adoption.is_finished(),
                "adopter must reach the gated job attachment"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("job attachment gate reached");
    let pruning = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .prune_completed_before(crate::time::now_ms(), 16)
                .await
        }
    });
    await_prune_skip_or_lock(&fixture, &pruning, adopter_pid).await;
    gate.commit().await.expect("release attachment gate");
    adoption.await.expect("adopter").expect("legacy adoption");
    let outcome = pruning.await.expect("pruner").expect("prune result");
    assert_eq!(
        outcome.jobs_deleted, 0,
        "a stale DELETE snapshot must not erase newly attached job custody"
    );
    assert_eq!(fixture.count("notification_outbox").await, 1);
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        1
    );
    fixture.close().await;
}

async fn collect_at(fixture: &IngressFixture, now: chrono::DateTime<chrono::Utc>) -> usize {
    gc_expired_aliases(
        &fixture.db,
        now,
        AliasGcBudget {
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
            lock_timeout: std::time::Duration::from_secs(1),
            statement_timeout: std::time::Duration::from_secs(2),
            scan_timeout: std::time::Duration::from_secs(2),
            progress: Default::default(),
        },
    )
    .await
    .expect("canonical GC")
    .deleted_messages
}

async fn eligibility(fixture: &IngressFixture, key: MessageKey) -> chrono::DateTime<chrono::Utc> {
    let sql = if fixture.db.driver() == crate::db::DatabaseDriver::Postgres {
        "SELECT to_char(retention_eligible_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM ingress_messages WHERE message_key = ?::uuid"
    } else {
        "SELECT retention_eligible_at FROM ingress_messages WHERE message_key = ?"
    };
    let conn = fixture.db.guard().await.expect("eligibility read");
    let mut rows = conn
        .query(sql, crate::db_params![key.to_storage().to_string()])
        .await
        .expect("eligibility row");
    let raw: String = rows
        .next()
        .await
        .expect("row")
        .expect("canonical row")
        .get(0)
        .expect("retention clock");
    chrono::DateTime::parse_from_rfc3339(&raw)
        .expect("retention timestamp")
        .with_timezone(&chrono::Utc)
}

async fn candidate_prune_waits_for_frontier_and_full_settlement_tail(fixture: IngressFixture) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let candidate = candidate("prune-frontier-tail");
    store.insert_candidate(&candidate).await.expect("candidate");
    let (key, intent) = parent(&fixture, &candidate).await;
    let mut tx = fixture.db.begin().await.expect("candidate custody");
    NotificationOutboxStore::attach_candidate_lineage_in_transaction(
        &mut tx,
        key,
        &intent.semantic_key(),
        &candidate,
    )
    .await
    .expect("attach custody");
    let id = NotificationOutboxStore::candidate_delivery_id_in_transaction(&mut tx, &candidate)
        .await
        .expect("delivery identity")
        .expect("candidate");
    tx.commit().await.expect("custody commit");
    store
        .execute("UPDATE notification_candidates SET outboxed_at_ms = 1", ())
        .await
        .expect("old outboxed candidate");
    let mut tx = fixture.uow.begin().await.expect("live stream ref");
    let stream = SmIngressStreamRepository::mint(&mut tx, &SmSessionId::new("prune-stream"))
        .await
        .expect("stream");
    SmIngressRepository::insert_sm_ref(
        &mut tx,
        stream,
        IngressOrdinal::FIRST,
        WireHandledCount::from_storage(1),
        key,
    )
    .await
    .expect("pending frontier ref");
    let settled = chrono::Utc::now();
    EffectDescendantRepository::settle_all(&mut tx, id, settled)
        .await
        .expect("settle candidate");
    tx.commit().await.expect("settlement commit");
    assert_eq!(
        collect_at(&fixture, settled + ALIAS_RETENTION).await,
        0,
        "a live frontier still protects canonical custody"
    );
    assert_eq!(
        store
            .prune_completed_before(crate::time::now_ms(), 16)
            .await
            .expect("frontier-protected prune")
            .candidates_deleted,
        0
    );
    let mut tx = fixture.uow.begin().await.expect("retire stream ref");
    SmIngressRepository::delete_stream_ref(&mut tx, stream, IngressOrdinal::FIRST)
        .await
        .expect("retire frontier ref");
    tx.commit().await.expect("frontier retirement commit");
    // The final frontier retirement starts its own complete eight-day tail.
    let retired = eligibility(&fixture, key).await;
    assert_eq!(collect_at(&fixture, retired).await, 0);
    assert_eq!(
        collect_at(
            &fixture,
            retired + ALIAS_RETENTION - chrono::Duration::microseconds(1)
        )
        .await,
        0
    );
    assert_eq!(
        store
            .prune_completed_before(crate::time::now_ms(), 16)
            .await
            .expect("full-tail prune")
            .candidates_deleted,
        0
    );
    assert_eq!(collect_at(&fixture, retired + ALIAS_RETENTION).await, 1);
    assert_eq!(
        store
            .prune_completed_before(crate::time::now_ms(), 16)
            .await
            .expect("expired prune")
            .candidates_deleted,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_candidate_prune_waits_for_frontier_and_full_settlement_tail() {
    candidate_prune_waits_for_frontier_and_full_settlement_tail(IngressFixture::sqlite().await)
        .await;
}

#[tokio::test]
async fn postgres_candidate_prune_waits_for_frontier_and_full_settlement_tail() {
    if let Some(fixture) = IngressFixture::postgres("candidate_prune_frontier_tail").await {
        candidate_prune_waits_for_frontier_and_full_settlement_tail(fixture).await;
    }
}

#[tokio::test]
async fn standalone_candidate_prune_preserves_null_identity_and_ordered_batch_contract() {
    let store = crate::notification_outbox::test_support::store().await;
    assert!(!store
        .has_canonical_descendants()
        .await
        .expect("standalone schema"));
    for id in ["first-null", "second", "quarantined", "pending"] {
        store
            .insert_candidate(&candidate(id))
            .await
            .expect("candidate");
    }
    store.execute("UPDATE notification_candidates SET delivery_id = NULL, outboxed_at_ms = 1 WHERE stanza_id = 'first-null'", ()).await.expect("legacy NULL identity");
    store
        .execute(
            "UPDATE notification_candidates SET outboxed_at_ms = 2 WHERE stanza_id = 'second'",
            (),
        )
        .await
        .expect("later completed candidate");
    store.execute("UPDATE notification_candidates SET outboxed_at_ms = 0, quarantined_at_ms = 1 WHERE stanza_id = 'quarantined'", ()).await.expect("protected quarantine");
    let first = store
        .prune_completed_before(3, 1)
        .await
        .expect("first batch");
    assert_eq!(first.candidates_deleted, 1);
    assert_eq!(store.count_all_candidates().await.expect("count"), 3);
    let mut rows = store
        .query(
            "SELECT stanza_id FROM notification_candidates WHERE stanza_id = 'first-null'",
            (),
        )
        .await
        .expect("NULL identity lookup");
    assert!(
        rows.next().await.expect("row").is_none(),
        "the complete tuple supports legacy NULL identities in age order"
    );
    drop(rows);
    assert_eq!(
        store
            .prune_completed_before(3, 1)
            .await
            .expect("second batch")
            .candidates_deleted,
        1
    );
    assert_eq!(
        store.count_all_candidates().await.expect("retained count"),
        2,
        "pending and quarantined candidates are protected"
    );
}
