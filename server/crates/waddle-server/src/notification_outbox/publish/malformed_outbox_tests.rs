use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_substrate::{AliasGcBudget, AliasGcProgress, MessageEnvelope};
use crate::ingress_uow::{
    CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
    EffectReceiptRepository,
};
use crate::notification_outbox::test_support::{candidate, enqueue_jobs_for_test, target_named};
use sha2::{Digest, Sha256};
use waddle_xmpp::ingress::{
    IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    SemanticDigest,
};

fn gc_budget() -> AliasGcBudget {
    AliasGcBudget {
        deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        lock_timeout: std::time::Duration::from_secs(1),
        statement_timeout: std::time::Duration::from_secs(2),
        scan_timeout: std::time::Duration::from_secs(2),
        progress: AliasGcProgress::default(),
    }
}

async fn pending_parent(fixture: &IngressFixture, job: &NotificationOutboxJob, index: u8) {
    let input = candidate(&format!("malformed-parent-{index}"));
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: input.recipient_bare_jid.clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: input.conversation_jid.clone(),
            archive_stanza_id: input.archive_stanza_id.clone(),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let mut message =
        xmpp_parsers::message::Message::new(Some(input.recipient_bare_jid.clone().into()));
    message.from = Some(input.sender_jid.clone());
    message.bodies.insert(
        xmpp_parsers::message::Lang::from(""),
        "approved notification".into(),
    );
    let key = MessageKey::new();
    let hash: [u8; 32] = Sha256::digest(intent.semantic_key().storage_identity().as_bytes()).into();
    let mut tx = fixture.uow.begin().await.expect("canonical parent");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [index; 32]).expect("digest"),
        Some(&MessageEnvelope::new(message)),
    )
    .await
    .expect("message");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("intent");
    EffectReceiptRepository::record_receipt(
        &mut tx,
        key,
        crate::ingress_substrate::EffectReceiptKind::from_storage(7),
        &hash,
    )
    .await
    .expect("receipt");
    CanonicalMessageRepository::terminalize(
        &mut tx,
        key,
        chrono::Utc::now() - chrono::Duration::days(20),
    )
    .await
    .expect("old terminal message");
    EffectDescendantRepository::attach(
        &mut tx,
        key,
        &intent.semantic_key(),
        uuid::Uuid::parse_str(job.job_id.as_str()).expect("job id"),
    )
    .await
    .expect("pending custody");
    tx.commit().await.expect("parent commit");
}

async fn malformed_prefix_preserves_unknown_custody_and_unblocks_neighbor(fixture: IngressFixture) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    for index in 0..3 {
        enqueue_jobs_for_test(
            &store,
            &candidate(&format!("bad-{index}")),
            &[target_named(&format!("bad-node-{index}"))],
        )
        .await;
    }
    enqueue_jobs_for_test(
        &store,
        &candidate("known-malformed"),
        &[target_named("known-malformed-node")],
    )
    .await;
    enqueue_jobs_for_test(
        &store,
        &candidate("valid-neighbor"),
        &[target_named("valid-node")],
    )
    .await;
    let jobs = store
        .pending_outbox_jobs()
        .await
        .expect("valid initial jobs");
    let payload = Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build();
    let options = Element::builder("x", waddle_xmpp::xep::NS_DATA_FORMS).build();
    for (index, job) in jobs
        .iter()
        .filter(|job| job.node.as_str().starts_with("bad-node"))
        .enumerate()
    {
        pending_parent(&fixture, job, index as u8 + 1).await;
        store.execute("UPDATE notification_outbox SET sender_jids = ?, queue_acceptance_may_exist = ?, approved_payload_xml = ?, approved_publish_options_xml = ?, created_at_ms = 1, updated_at_ms = 1 WHERE job_id = ?", crate::db_params!["not-json", 1_i64, String::from(&payload), String::from(&options), job.job_id.as_str()]).await.expect("malformed queued prefix");
    }
    let known = jobs
        .iter()
        .find(|job| job.node.as_str() == "known-malformed-node")
        .expect("known malformed job");
    pending_parent(&fixture, known, 4).await;
    store.execute("UPDATE notification_outbox SET sender_jids = ?, approved_payload_xml = ?, approved_publish_options_xml = ?, created_at_ms = 2, updated_at_ms = 1 WHERE job_id = ?", crate::db_params!["not-json", String::from(&payload), String::from(&options), known.job_id.as_str()]).await.expect("known unaccepted malformed positive control");
    store
        .execute(
            "UPDATE notification_outbox SET created_at_ms = 3 WHERE node = ?",
            crate::db_params!["valid-node"],
        )
        .await
        .expect("later neighbor");
    assert!(store
        .claim_due_outbox_jobs(3)
        .await
        .expect("quarantine full prefix")
        .is_empty());
    assert_eq!(
        fixture
            .count("notification_outbox WHERE status = 'failed'")
            .await,
        3,
        "malformed uncertainty must leave the due queue without releasing custody"
    );
    assert_eq!(
        fixture
            .count("notification_outbox WHERE status = 'failed' AND queue_acceptance_may_exist = 1")
            .await,
        3
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        4
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NOT NULL")
            .await,
        0,
        "unknown malformed quarantine must not settle any custody"
    );
    let next = store
        .claim_due_outbox_jobs(3)
        .await
        .expect("valid neighbor progresses");
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].node.as_str(), "valid-node");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        3
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NOT NULL")
            .await,
        1,
        "known never-accepted malformed work may settle"
    );
    let mut rows = store.query("SELECT approved_payload_xml, approved_publish_options_xml, last_error FROM notification_outbox WHERE status = 'failed'", ()).await.expect("quarantine audit");
    while let Some(row) = rows.next().await.expect("row") {
        assert_eq!(
            row.get::<String>(0).expect("payload"),
            String::from(&payload)
        );
        assert_eq!(
            row.get::<String>(1).expect("options"),
            String::from(&options)
        );
        assert!(row.get::<String>(2).expect("cause").contains("sender"));
    }
    drop(rows);
    store
        .execute(
            "UPDATE notification_outbox SET updated_at_ms = 1 WHERE status = 'failed'",
            (),
        )
        .await
        .expect("old quarantine");
    store
        .prune_completed_before(crate::time::now_ms(), 16)
        .await
        .expect("prune quarantine");
    assert_eq!(
        fixture
            .count("notification_outbox WHERE status = 'failed' AND queue_acceptance_may_exist = 1")
            .await,
        3
    );
    assert_eq!(
        crate::ingress_substrate::gc_expired_aliases(&fixture.db, chrono::Utc::now(), gc_budget())
            .await
            .expect("GC keeps unknown evidence")
            .deleted_messages,
        0
    );
    drop(store);
    fixture.close().await;
}

async fn orphaned_failed_unknown_row_keeps_its_evidence(fixture: IngressFixture) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    enqueue_jobs_for_test(
        &store,
        &candidate("orphaned-unknown"),
        &[target_named("unknown-node")],
    )
    .await;
    store
        .execute("DELETE FROM notification_outbox_lineage", ())
        .await
        .expect("unclassified legacy row without lineage");
    store.execute("UPDATE notification_outbox SET status = ?, queue_acceptance_may_exist = 1, updated_at_ms = 1", crate::db_params![STATUS_FAILED]).await.expect("legacy uncertain failure");
    assert_eq!(
        store
            .prune_completed_before(crate::time::now_ms(), 16)
            .await
            .expect("uncertain legacy retention")
            .jobs_deleted,
        0,
        "local queue/ancestry absence is not a terminal disposition for foreign acceptance"
    );
    enqueue_jobs_for_test(
        &store,
        &candidate("known-terminal"),
        &[target_named("known-node")],
    )
    .await;
    store.execute("DELETE FROM notification_outbox_lineage WHERE job_id IN (SELECT job_id FROM notification_outbox WHERE node = ?)", crate::db_params!["known-node"]).await.expect("unowned known terminal");
    store
        .execute(
            "UPDATE notification_outbox SET status = ?, updated_at_ms = 1 WHERE node = ?",
            crate::db_params![STATUS_FAILED, "known-node"],
        )
        .await
        .expect("known never accepted");
    assert_eq!(
        store
            .prune_completed_before(crate::time::now_ms(), 16)
            .await
            .expect("known terminal positive control")
            .jobs_deleted,
        1
    );
    assert_eq!(fixture.count("notification_outbox WHERE node = 'unknown-node' AND queue_acceptance_may_exist = 1").await, 1);
    enqueue_jobs_for_test(
        &store,
        &candidate("published-terminal"),
        &[target_named("published-node")],
    )
    .await;
    store.execute("DELETE FROM notification_outbox_lineage WHERE job_id IN (SELECT job_id FROM notification_outbox WHERE node = ?)", crate::db_params!["published-node"]).await.expect("settled publication fixture");
    store.execute("UPDATE notification_outbox SET status = ?, queue_acceptance_may_exist = 1, updated_at_ms = 1 WHERE node = ?", crate::db_params![STATUS_PUBLISHED, "published-node"]).await.expect("accepted published positive control");
    assert_eq!(
        store
            .prune_completed_before(crate::time::now_ms(), 16)
            .await
            .expect("published marker1 remains normally prunable")
            .jobs_deleted,
        1
    );
    assert_eq!(
        fixture
            .count("notification_outbox WHERE node = 'unknown-node'")
            .await,
        1
    );
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_malformed_unknown_prefix_does_not_starve_valid_jobs() {
    malformed_prefix_preserves_unknown_custody_and_unblocks_neighbor(
        IngressFixture::sqlite().await,
    )
    .await;
}
#[tokio::test]
async fn postgres_malformed_unknown_prefix_does_not_starve_valid_jobs() {
    if let Some(f) = IngressFixture::postgres("malformed_due_prefix").await {
        malformed_prefix_preserves_unknown_custody_and_unblocks_neighbor(f).await;
    }
}
#[tokio::test]
async fn sqlite_failed_unknown_legacy_row_is_not_pruned() {
    orphaned_failed_unknown_row_keeps_its_evidence(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_failed_unknown_legacy_row_is_not_pruned() {
    if let Some(f) = IngressFixture::postgres("failed_unknown_prune").await {
        orphaned_failed_unknown_row_keeps_its_evidence(f).await;
    }
}

async fn malformed_snapshot_cannot_overwrite_concurrent_repair_or_claim(
    fixture: IngressFixture,
    repair: bool,
) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    enqueue_jobs_for_test(
        &store,
        &candidate("malformed-snapshot"),
        &[target_named("snapshot-node")],
    )
    .await;
    let job = store.pending_outbox_jobs().await.expect("job").remove(0);
    let valid_senders = encode_sender_jids(&job.sender_jids).expect("senders");
    store
        .execute(
            "UPDATE notification_outbox SET sender_jids = ? WHERE job_id = ?",
            crate::db_params!["not-json", job.job_id.as_str()],
        )
        .await
        .expect("malformed initial snapshot");
    let mut change = fixture.db.begin().await.expect("concurrent current owner");
    let successor_token = uuid::Uuid::new_v4().to_string();
    if repair {
        change
            .execute(
                "UPDATE notification_outbox SET sender_jids = ? WHERE job_id = ?",
                crate::db_params![valid_senders, job.job_id.as_str()],
            )
            .await
            .expect("repair before quarantine lock");
    } else {
        change.execute("UPDATE notification_outbox SET status = ?, claim_token = ?, claimed_at_ms = ?, updated_at_ms = ? WHERE job_id = ?", crate::db_params![STATUS_IN_PROGRESS, successor_token.as_str(), crate::time::now_ms(), crate::time::now_ms(), job.job_id.as_str()]).await.expect("successor owns row");
    }
    let mut rows = change
        .query("SELECT txid_current()::text", ())
        .await
        .expect("transaction id");
    let xid: String = rows
        .next()
        .await
        .expect("row")
        .expect("xid")
        .get(0)
        .expect("value");
    drop(rows);
    let claim_store = store.clone();
    let claims = tokio::spawn(async move { claim_store.claim_due_outbox_jobs(1).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let conn = fixture.db.guard().await.expect("lock observer");
            let mut rows = conn.query("SELECT COUNT(*) FROM pg_locks WHERE locktype = 'transactionid' AND transactionid::text = ? AND NOT granted", crate::db_params![xid.as_str()]).await.expect("snapshot wait");
            let waiting: i64 = rows.next().await.expect("row").expect("count").get(0).expect("value");
            if waiting > 0 { break; }
            drop(rows);
            drop(conn);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("malformed handler waited on exact current-row transaction");
    change
        .commit()
        .await
        .expect("concurrent change becomes visible");
    assert!(claims
        .await
        .expect("claim task")
        .expect("fenced quarantine")
        .is_empty());
    if repair {
        assert_eq!(
            fixture
                .count("notification_outbox WHERE status = 'queued' AND last_error IS NULL")
                .await,
            1,
            "a repaired job must not be failed from an old malformed snapshot"
        );
        assert_eq!(
            store
                .claim_due_outbox_jobs(1)
                .await
                .expect("repaired job progresses")
                .len(),
            1
        );
    } else {
        let mut rows = store.query("SELECT status, claim_token, claimed_at_ms FROM notification_outbox WHERE job_id = ?", crate::db_params![job.job_id.as_str()]).await.expect("successor claim");
        let row = rows.next().await.expect("row").expect("job");
        assert_eq!(row.get::<String>(0).expect("status"), STATUS_IN_PROGRESS);
        assert_eq!(row.get::<String>(1).expect("token"), successor_token);
        assert!(row.get::<Option<i64>>(2).expect("lease").is_some());
        drop(rows);
        assert!(store
            .claim_due_outbox_jobs(1)
            .await
            .expect("fresh malformed lease is not due")
            .is_empty());
    }
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        1
    );
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn postgres_malformed_snapshot_cannot_fail_repaired_job() {
    if let Some(f) = IngressFixture::postgres("malformed_repair_fence").await {
        malformed_snapshot_cannot_overwrite_concurrent_repair_or_claim(f, true).await;
    }
}
#[tokio::test]
async fn postgres_malformed_snapshot_cannot_clear_successor_claim() {
    if let Some(f) = IngressFixture::postgres("malformed_claim_fence").await {
        malformed_snapshot_cannot_overwrite_concurrent_repair_or_claim(f, false).await;
    }
}
