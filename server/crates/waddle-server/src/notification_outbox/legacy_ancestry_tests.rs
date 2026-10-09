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
    let key = MessageKey::new();
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: candidate.recipient_bare_jid.clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: candidate.conversation_jid.clone(),
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
