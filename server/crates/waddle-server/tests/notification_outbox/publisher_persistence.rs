//! Publisher persistence boundaries exercised through the live scheduler.

use crate::ingress_support::IngressFixture;
use crate::support::{
    bare, candidate, enqueue_jobs_for_test, inbox_with_unread, register_push_target,
    FailingBlockingStorage, TestStore,
};
use waddle_server::db::{Database, DatabaseDriver};
use waddle_server::ingress_uow::{CanonicalMessageRepository, EffectIntentRepository};
use waddle_server::notification_outbox::{
    NotificationCandidate, NotificationOutboxJob, NotificationOutboxPublishOutcome,
    NotificationOutboxStatus, NotificationOutboxStore, NotificationOutboxTarget,
    PushServiceNodeName,
};
use waddle_server::push_registrations::DatabasePushRegistrationStore;
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_server::test_support::attach_notification_candidate_lineage_for_test;
use waddle_xmpp::ingress::{
    IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    SemanticDigest,
};
use waddle_xmpp::push::InMemoryPushStore;
use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

async fn publisher_setup(
    fixture: &IngressFixture,
) -> (
    TestStore,
    DatabasePushServiceStore,
    InMemoryPushStore,
    NotificationOutboxTarget,
) {
    let store = TestStore {
        db: fixture.db.clone(),
        store: NotificationOutboxStore::try_new(fixture.db.clone())
            .await
            .expect("outbox"),
    };
    let push = DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("publisher-boundary-provider")
            .await
            .expect("provider database"),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push service");
    DatabasePushRegistrationStore::try_new(push.database())
        .await
        .expect("registration schema");
    let owner = bare("alice@example.com");
    let node = push.ensure_node(&owner, "web").await.expect("node");
    push.upsert_device(
        &owner,
        PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
    )
    .await
    .expect("device");
    push.register_first_party_node_for_owner(&owner, "push.example.com", node.node(), None)
        .await
        .expect("first-party registration");
    let target = NotificationOutboxTarget::new(
        bare("push.example.com"),
        PushServiceNodeName::try_new(node.node()).expect("node name"),
    );
    let registrations = InMemoryPushStore::new();
    register_push_target(&registrations, &owner, &target).await;
    (store, push, registrations, target)
}

async fn attach_parent(fixture: &IngressFixture, candidate: &NotificationCandidate) {
    let key = MessageKey::new();
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: candidate.recipient_bare_jid().clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: candidate.conversation_jid().clone(),
            archive_stanza_id: candidate.archive_stanza_id().clone(),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let mut tx = fixture.uow.begin().await.expect("canonical transaction");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [31; 32]).expect("digest"),
        None,
    )
    .await
    .expect("record parent");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("record intent");
    tx.commit().await.expect("commit parent");
    attach_notification_candidate_lineage_for_test(
        &fixture.uow,
        key,
        &intent.semantic_key(),
        candidate,
    )
    .await
    .expect("attach lineage");
}

async fn claim(store: &TestStore) -> NotificationOutboxJob {
    let mut jobs = store.claim_due_outbox_jobs(1).await.expect("claim");
    assert_eq!(jobs.len(), 1);
    jobs.remove(0)
}

async fn stale_claim_cannot_publish(fixture: IngressFixture) {
    let (store, push, registrations, target) = publisher_setup(&fixture).await;
    enqueue_jobs_for_test(&store, &candidate("publisher-stale"), &[target]).await;
    let stale = claim(&store).await;
    fixture
        .execute("UPDATE notification_outbox SET claimed_at_ms = 1", ())
        .await;
    assert!(store
        .claim_due_outbox_jobs(1)
        .await
        .expect("expire claim")
        .is_empty());
    fixture
        .execute("UPDATE notification_outbox SET next_attempt_at_ms = 1", ())
        .await;
    let fresh = claim(&store).await;
    assert_ne!(stale.claim_token(), fresh.claim_token());
    let inbox = inbox_with_unread(&bare("alice@example.com"), &bare("bob@example.com"), 1).await;
    let blocking = InMemoryBlockingStorage::new();
    let stale_outcome = store
        .publish_claimed_job(
            &stale,
            &push,
            &registrations,
            &inbox,
            &blocking,
            &bare("push.example.com"),
        )
        .await
        .expect("stale publish");
    assert!(matches!(
        stale_outcome,
        NotificationOutboxPublishOutcome::RetryScheduled { .. }
    ));
    assert!(
        !store
            .mark_job_published(&stale)
            .await
            .expect("stale completion compare-and-swap"),
        "a stale worker cannot complete the fresh claim"
    );
    let pending = store.pending_outbox_jobs().await.expect("current claim");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status(), NotificationOutboxStatus::InProgress);
    assert_eq!(pending[0].claim_token(), fresh.claim_token());
    assert_eq!(
        fixture
            .count("notification_outbox WHERE published_at_ms IS NOT NULL")
            .await,
        0
    );
    assert!(push
        .queued_publish_jobs()
        .await
        .expect("provider queue")
        .is_empty());
    let fresh_outcome = store
        .publish_claimed_job(
            &fresh,
            &push,
            &registrations,
            &inbox,
            &blocking,
            &bare("push.example.com"),
        )
        .await
        .expect("fresh publish");
    assert!(matches!(
        fresh_outcome,
        NotificationOutboxPublishOutcome::Published { .. }
    ));
    assert_eq!(fixture.count("notification_outbox WHERE status = 'published' AND claim_token IS NULL AND claimed_at_ms IS NULL AND published_at_ms IS NOT NULL").await, 1);
    assert_eq!(
        push.queued_publish_jobs()
            .await
            .expect("provider queue")
            .len(),
        1
    );
    drop(store);
    fixture.close().await;
}

async fn install_settlement_failure(fixture: &IngressFixture) {
    match fixture.db.driver() {
        DatabaseDriver::Sqlite => fixture.execute(
            "CREATE TRIGGER fail_publisher_settlement BEFORE UPDATE OF settled_at_ms ON notification_outbox_lineage BEGIN SELECT RAISE(FAIL, 'publisher settlement failed'); END",
            (),
        ).await,
        DatabaseDriver::Postgres => {
            fixture.execute(
                "CREATE FUNCTION fail_publisher_settlement() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'publisher settlement failed'; END $$",
                (),
            ).await;
            fixture.execute(
                "CREATE TRIGGER fail_publisher_settlement BEFORE UPDATE OF settled_at_ms ON notification_outbox_lineage FOR EACH ROW EXECUTE FUNCTION fail_publisher_settlement()",
                (),
            ).await;
        }
    }
}

async fn remove_settlement_failure(fixture: &IngressFixture) {
    match fixture.db.driver() {
        DatabaseDriver::Sqlite => {
            fixture
                .execute("DROP TRIGGER fail_publisher_settlement", ())
                .await
        }
        DatabaseDriver::Postgres => {
            fixture
                .execute(
                    "DROP TRIGGER fail_publisher_settlement ON notification_outbox_lineage",
                    (),
                )
                .await;
            fixture
                .execute("DROP FUNCTION fail_publisher_settlement()", ())
                .await;
        }
    }
}

async fn terminal_transaction_rolls_back(fixture: IngressFixture) {
    let (store, push, registrations, target) = publisher_setup(&fixture).await;
    let candidate = candidate("publisher-rollback");
    store.insert_candidate(&candidate).await.expect("candidate");
    attach_parent(&fixture, &candidate).await;
    enqueue_jobs_for_test(&store, &candidate, &[target]).await;
    let job = claim(&store).await;
    let inbox = inbox_with_unread(&bare("alice@example.com"), &bare("bob@example.com"), 1).await;
    let blocking = InMemoryBlockingStorage::new();
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        1
    );
    install_settlement_failure(&fixture).await;
    // Invalid-target refusal first updates the job, then settles its canonical
    // descendant, then updates lineage. Fail the last statement of that unit.
    let error = store
        .publish_claimed_job(
            &job,
            &push,
            &registrations,
            &inbox,
            &blocking,
            &bare("other-push.example.com"),
        )
        .await
        .expect_err("settlement trigger must abort terminal transaction");
    assert!(
        error.to_string().contains("publisher settlement failed"),
        "{error}"
    );
    assert_eq!(fixture.count("notification_outbox WHERE status = 'in-progress' AND claim_token IS NOT NULL AND last_error IS NULL AND published_at_ms IS NULL AND policy_error_count = 0 AND attempt_count = 0").await, 1);
    assert_eq!(
        store.pending_outbox_jobs().await.expect("retained claim")[0].claim_token(),
        job.claim_token()
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        1
    );
    assert!(push
        .queued_publish_jobs()
        .await
        .expect("provider queue")
        .is_empty());
    remove_settlement_failure(&fixture).await;
    let outcome = store
        .publish_claimed_job(
            &job,
            &push,
            &registrations,
            &inbox,
            &blocking,
            &bare("other-push.example.com"),
        )
        .await
        .expect("retry terminal transaction");
    assert!(matches!(
        outcome,
        NotificationOutboxPublishOutcome::Failed { .. }
    ));
    assert_eq!(fixture.count("notification_outbox WHERE status = 'failed' AND claim_token IS NULL AND claimed_at_ms IS NULL AND next_attempt_at_ms IS NULL AND published_at_ms IS NULL AND queue_acceptance_may_exist = 0 AND last_error = 'invalid_target'").await, 1);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        0
    );
    drop(store);
    fixture.close().await;
}

async fn retry_and_terminal_durable_state(fixture: IngressFixture) {
    let (store, push, registrations, target) = publisher_setup(&fixture).await;
    let candidate = candidate("publisher-policy-state");
    store.insert_candidate(&candidate).await.expect("candidate");
    attach_parent(&fixture, &candidate).await;
    enqueue_jobs_for_test(&store, &candidate, &[target]).await;
    let inbox = inbox_with_unread(&bare("alice@example.com"), &bare("bob@example.com"), 1).await;
    let before = waddle_server::time::now_ms();
    let outcomes = store
        .drain_due_outbox_jobs(
            &push,
            &registrations,
            &inbox,
            &FailingBlockingStorage,
            &bare("push.example.com"),
            1,
        )
        .await
        .expect("retry outcome");
    let after = waddle_server::time::now_ms();
    assert!(matches!(
        outcomes.as_slice(),
        [NotificationOutboxPublishOutcome::RetryScheduled { .. }]
    ));
    assert_eq!(fixture.count("notification_outbox WHERE status = 'queued' AND attempt_count = 0 AND policy_error_count = 1 AND last_error = 'policy_lookup' AND claim_token IS NULL AND claimed_at_ms IS NULL AND published_at_ms IS NULL AND queue_acceptance_may_exist = 0 AND approved_payload_xml IS NULL AND approved_publish_options_xml IS NULL").await, 1);
    let mut rows = store
        .query("SELECT next_attempt_at_ms FROM notification_outbox", ())
        .await
        .expect("deadline");
    let deadline: i64 = rows
        .next()
        .await
        .expect("row")
        .expect("job")
        .get(0)
        .expect("deadline value");
    assert!(deadline >= before + 45_000 && deadline <= after + 75_000);
    drop(rows);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        1
    );
    fixture
        .execute(
            "UPDATE notification_outbox SET policy_error_count = 4, next_attempt_at_ms = 1",
            (),
        )
        .await;
    let outcomes = store
        .drain_due_outbox_jobs(
            &push,
            &registrations,
            &inbox,
            &FailingBlockingStorage,
            &bare("push.example.com"),
            1,
        )
        .await
        .expect("terminal policy outcome");
    assert!(matches!(
        outcomes.as_slice(),
        [NotificationOutboxPublishOutcome::Failed { .. }]
    ));
    assert_eq!(fixture.count("notification_outbox WHERE status = 'failed' AND attempt_count = 0 AND policy_error_count = 5 AND last_error = 'policy_lookup' AND next_attempt_at_ms IS NULL AND claim_token IS NULL AND claimed_at_ms IS NULL AND published_at_ms IS NULL AND queue_acceptance_may_exist = 0 AND approved_payload_xml IS NULL AND approved_publish_options_xml IS NULL").await, 1);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        0
    );
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("no pending jobs")
        .is_empty());
    assert!(push
        .queued_publish_jobs()
        .await
        .expect("provider queue")
        .is_empty());
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_stale_claim_cannot_publish_reclaimed_job() {
    stale_claim_cannot_publish(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_stale_claim_cannot_publish_reclaimed_job() {
    if let Some(fixture) = IngressFixture::postgres("publisher_stale").await {
        stale_claim_cannot_publish(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_terminal_transaction_rolls_back_all_statements() {
    terminal_transaction_rolls_back(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_terminal_transaction_rolls_back_all_statements() {
    if let Some(fixture) = IngressFixture::postgres("publisher_rollback").await {
        terminal_transaction_rolls_back(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_retry_and_terminal_outcomes_have_exact_durable_state() {
    retry_and_terminal_durable_state(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_retry_and_terminal_outcomes_have_exact_durable_state() {
    if let Some(fixture) = IngressFixture::postgres("publisher_retry").await {
        retry_and_terminal_durable_state(fixture).await;
    }
}

#[derive(Clone, Copy)]
enum RollbackOperation {
    Published,
    Suppression,
    TerminalRetry,
    GoneRegistration,
    TerminalProof,
}

// Snapshot every column, including claim/deadline timestamps and frozen bytes,
// rather than checking only the status changed by the transaction's first write.
async fn job_snapshot(store: &TestStore) -> Vec<Vec<waddle_server::db::Value>> {
    let mut rows = store
        .query("SELECT * FROM notification_outbox ORDER BY job_id", ())
        .await
        .expect("durable job snapshot");
    let width = rows.column_count();
    let mut snapshot = Vec::new();
    while let Some(row) = rows.next().await.expect("snapshot row") {
        snapshot.push((0..width).map(|i| row.value(i).expect("column")).collect());
    }
    snapshot
}

async fn assert_custody(fixture: &IngressFixture, unsettled: i64) {
    // The candidate descendant was settled by T1; the outbox descendant is
    // the remaining obligation exercised by the publisher transaction.
    assert_eq!(fixture.count("ingress_effect_descendants").await, 2);
    assert_eq!(fixture.count("notification_outbox_lineage").await, 1);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        unsettled,
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        unsettled,
    );
}

async fn install_ack_failure(fixture: &IngressFixture) {
    match fixture.db.driver() {
        DatabaseDriver::Sqlite => fixture.execute(
            "CREATE TRIGGER fail_publisher_ack BEFORE UPDATE OF upstream_completed ON push_publish_jobs BEGIN SELECT RAISE(FAIL, 'publisher acknowledgement failed'); END",
            (),
        ).await,
        DatabaseDriver::Postgres => {
            fixture.execute(
                "CREATE FUNCTION fail_publisher_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'publisher acknowledgement failed'; END $$",
                (),
            ).await;
            fixture.execute(
                "CREATE TRIGGER fail_publisher_ack BEFORE UPDATE OF upstream_completed ON push_publish_jobs FOR EACH ROW EXECUTE FUNCTION fail_publisher_ack()",
                (),
            ).await;
        }
    }
}

async fn remove_ack_failure(fixture: &IngressFixture) {
    match fixture.db.driver() {
        DatabaseDriver::Sqlite => fixture.execute("DROP TRIGGER fail_publisher_ack", ()).await,
        DatabaseDriver::Postgres => {
            fixture
                .execute("DROP TRIGGER fail_publisher_ack ON push_publish_jobs", ())
                .await;
            fixture
                .execute("DROP FUNCTION fail_publisher_ack()", ())
                .await;
        }
    }
}

async fn run_rollback_operation(
    operation: RollbackOperation,
    store: &TestStore,
    push: &DatabasePushServiceStore,
    job: &NotificationOutboxJob,
) -> Result<(), waddle_server::notification_outbox::NotificationOutboxError> {
    match operation {
        RollbackOperation::Published => assert!(store.mark_job_published(job).await?),
        RollbackOperation::TerminalRetry => assert_eq!(
            store
                .schedule_retry_or_fail(job, "rollback retry".into())
                .await?,
            Some(4),
        ),
        _ => {
            let registrations = InMemoryPushStore::new();
            if !matches!(operation, RollbackOperation::GoneRegistration) {
                register_push_target(
                    &registrations,
                    job.recipient_bare_jid(),
                    &NotificationOutboxTarget::new(
                        job.push_service_jid().clone(),
                        job.node().clone(),
                    ),
                )
                .await;
            }
            // An existing zero-unread entry is required: an absent entry does
            // not suppress. The other paths retain one unread message.
            let (inbox, _) = crate::support::seed_inbox_witness(
                job.recipient_bare_jid(),
                job.conversation_jid(),
                "read-before-publish",
                1,
                u32::from(!matches!(operation, RollbackOperation::Suppression)),
            )
            .await;
            let outcome = store
                .publish_claimed_job(
                    job,
                    push,
                    &registrations,
                    &inbox,
                    &InMemoryBlockingStorage::new(),
                    &bare("push.example.com"),
                )
                .await?;
            assert!(matches!(
                (operation, outcome),
                (
                    RollbackOperation::Suppression,
                    NotificationOutboxPublishOutcome::Suppressed { .. }
                ) | (
                    RollbackOperation::GoneRegistration,
                    NotificationOutboxPublishOutcome::Failed { .. }
                ) | (
                    RollbackOperation::TerminalProof,
                    NotificationOutboxPublishOutcome::Published { .. }
                )
            ));
        }
    }
    Ok(())
}

async fn publisher_transaction_rolls_back(fixture: IngressFixture, operation: RollbackOperation) {
    let store = TestStore {
        db: fixture.db.clone(),
        store: NotificationOutboxStore::try_new(fixture.db.clone())
            .await
            .expect("outbox"),
    };
    // Colocation makes the real provider acknowledgement a write in the SAME
    // transaction, so its trigger also tests rollback of preceding settlement.
    let push = DatabasePushServiceStore::new_with_secret_key(
        fixture.db.clone(),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("colocated provider");
    let owner = bare("alice@example.com");
    let node = push
        .ensure_node(&owner, "publisher-rollback")
        .await
        .expect("node");
    let target = NotificationOutboxTarget::new(
        bare("push.example.com"),
        PushServiceNodeName::try_new(node.node()).expect("node name"),
    );
    let candidate = candidate("publisher-atomic-operation");
    store.insert_candidate(&candidate).await.expect("candidate");
    attach_parent(&fixture, &candidate).await;
    enqueue_jobs_for_test(&store, &candidate, &[target]).await;
    fixture.execute(
        "UPDATE notification_outbox SET attempt_count = ?, policy_error_count = 1, last_error = 'before transition', next_attempt_at_ms = 1",
        waddle_server::db_params![if matches!(operation, RollbackOperation::TerminalRetry) { 3_i64 } else { 2_i64 }],
    ).await;
    let mut job = claim(&store).await;
    let with_ack = matches!(
        operation,
        RollbackOperation::Published | RollbackOperation::TerminalProof
    );
    if with_ack {
        let item = job.to_xep0357_pubsub_item();
        fixture.execute(
            "UPDATE notification_outbox SET approved_payload_xml = ?, queue_acceptance_may_exist = 1",
            waddle_server::db_params![String::from(item.payload.as_ref().expect("notification payload"))],
        ).await;
        // Seed the exact durable provider acceptance/proof that the public
        // completion paths consume, without first settling the owner in setup.
        fixture.execute(
            "INSERT INTO push_publish_jobs (job_id, owner_bare_jid, push_service_jid, node, item_id, payload_xml, publish_options_xml, ancestry_job_id, acceptance_scope, status, created_at_ms, updated_at_ms) SELECT 'publisher-acceptance', recipient_bare_jid, push_service_jid, node, job_id, approved_payload_xml, approved_publish_options_xml, job_id, 'canonical', ?, 1, 1 FROM notification_outbox",
            waddle_server::db_params![if matches!(operation, RollbackOperation::TerminalProof) { "published" } else { "queued" }],
        ).await;
        job = store
            .pending_outbox_jobs()
            .await
            .expect("frozen claim")
            .remove(0);
        install_ack_failure(&fixture).await;
    } else {
        install_settlement_failure(&fixture).await;
    }
    let before = job_snapshot(&store).await;
    assert_eq!(before.len(), 1);
    assert_custody(&fixture, 1).await;
    assert_eq!(
        fixture
            .count("push_publish_jobs WHERE upstream_completed = 1")
            .await,
        0
    );
    let error = run_rollback_operation(operation, &store, &push, &job)
        .await
        .expect_err("late write must abort the whole operation");
    if with_ack {
        // The ancestry boundary deliberately redacts the provider DB error.
        assert!(matches!(
            error,
            waddle_server::notification_outbox::NotificationOutboxError::Ancestry(_)
        ));
    } else {
        assert!(
            error.to_string().contains("publisher settlement failed"),
            "{error}"
        );
    }
    assert_eq!(
        job_snapshot(&store).await,
        before,
        "every job column must roll back"
    );
    assert_custody(&fixture, 1).await;
    assert_eq!(
        fixture
            .count("push_publish_jobs WHERE upstream_completed = 1")
            .await,
        0
    );
    assert_eq!(
        fixture.count("push_publish_jobs").await,
        i64::from(with_ack)
    );
    if with_ack {
        remove_ack_failure(&fixture).await;
    } else {
        remove_settlement_failure(&fixture).await;
    }
    run_rollback_operation(operation, &store, &push, &job)
        .await
        .expect("retry commits");
    match operation {
        RollbackOperation::Suppression => assert!(job_snapshot(&store).await.is_empty()),
        RollbackOperation::Published | RollbackOperation::TerminalProof => {
            assert_eq!(fixture.count("notification_outbox WHERE status = 'published' AND attempt_count = 2 AND policy_error_count = 0 AND last_error IS NULL AND claim_token IS NULL AND claimed_at_ms IS NULL AND next_attempt_at_ms IS NULL AND published_at_ms IS NOT NULL AND queue_acceptance_may_exist = 1 AND approved_payload_xml IS NOT NULL").await, 1);
        }
        RollbackOperation::TerminalRetry => {
            assert_eq!(fixture.count("notification_outbox WHERE status = 'failed' AND attempt_count = 4 AND policy_error_count = 1 AND last_error = 'unknown' AND claim_token IS NULL AND claimed_at_ms IS NULL AND next_attempt_at_ms IS NULL AND published_at_ms IS NULL AND queue_acceptance_may_exist = 0 AND approved_payload_xml IS NULL AND approved_publish_options_xml IS NULL").await, 1);
        }
        RollbackOperation::GoneRegistration => {
            assert_eq!(fixture.count("notification_outbox WHERE status = 'failed' AND attempt_count = 2 AND policy_error_count = 0 AND last_error = 'registration_gone' AND claim_token IS NULL AND claimed_at_ms IS NULL AND next_attempt_at_ms IS NULL AND published_at_ms IS NULL AND queue_acceptance_may_exist = 0 AND approved_payload_xml IS NULL AND approved_publish_options_xml IS NULL").await, 1);
        }
    }
    // mark_job_published transfers upstream ownership; its still-queued provider
    // retains ancestry. Terminal proof and unowned terminal paths settle it.
    assert_custody(
        &fixture,
        i64::from(matches!(operation, RollbackOperation::Published)),
    )
    .await;
    assert_eq!(
        fixture
            .count("push_publish_jobs WHERE upstream_completed = 1")
            .await,
        i64::from(with_ack)
    );
    assert_eq!(
        fixture.count("push_publish_jobs").await,
        i64::from(with_ack)
    );
    drop(push);
    drop(store);
    fixture.close().await;
}

// mark_job_published: ancestry locks, job UPDATE, matching provider SELECT,
// provider acknowledgement UPDATE (failed). No ancestry settlement in this path.
#[tokio::test]
async fn sqlite_mark_job_published_rolls_back_all_statements() {
    publisher_transaction_rolls_back(IngressFixture::sqlite().await, RollbackOperation::Published)
        .await;
}

// Same statement order and acknowledgement failure as the SQLite guard.
#[tokio::test]
async fn postgres_mark_job_published_rolls_back_all_statements() {
    if let Some(fixture) = IngressFixture::postgres("publisher_published_rollback").await {
        publisher_transaction_rolls_back(fixture, RollbackOperation::Published).await;
    }
}

// persist_claimed_job_suppression: ancestry locks, job DELETE, unowned-provider
// check, canonical descendant UPDATE, lineage UPDATE (failed).
#[tokio::test]
async fn sqlite_suppression_rolls_back_all_statements() {
    publisher_transaction_rolls_back(
        IngressFixture::sqlite().await,
        RollbackOperation::Suppression,
    )
    .await;
}

// Same DELETE, descendant UPDATE, then failed lineage UPDATE as SQLite.
#[tokio::test]
async fn postgres_suppression_rolls_back_all_statements() {
    if let Some(fixture) = IngressFixture::postgres("publisher_suppression_rollback").await {
        publisher_transaction_rolls_back(fixture, RollbackOperation::Suppression).await;
    }
}

// persist_claimed_job_retry (terminal): ancestry locks, acceptance SELECT, job
// UPDATE, unowned-provider check, descendant UPDATE, lineage UPDATE (failed).
#[tokio::test]
async fn sqlite_terminal_retry_rolls_back_all_statements() {
    publisher_transaction_rolls_back(
        IngressFixture::sqlite().await,
        RollbackOperation::TerminalRetry,
    )
    .await;
}

// Same acceptance SELECT, job/descendant UPDATEs, failed lineage UPDATE as SQLite.
#[tokio::test]
async fn postgres_terminal_retry_rolls_back_all_statements() {
    if let Some(fixture) = IngressFixture::postgres("publisher_terminal_retry_rollback").await {
        publisher_transaction_rolls_back(fixture, RollbackOperation::TerminalRetry).await;
    }
}

// persist_gone_registration_failure: ancestry locks, acceptance SELECT, job
// UPDATE, unowned-provider check, descendant UPDATE, lineage UPDATE (failed).
#[tokio::test]
async fn sqlite_gone_registration_rolls_back_all_statements() {
    publisher_transaction_rolls_back(
        IngressFixture::sqlite().await,
        RollbackOperation::GoneRegistration,
    )
    .await;
}

// Same acceptance SELECT, job/descendant UPDATEs, failed lineage UPDATE as SQLite.
#[tokio::test]
async fn postgres_gone_registration_rolls_back_all_statements() {
    if let Some(fixture) = IngressFixture::postgres("publisher_gone_rollback").await {
        publisher_transaction_rolls_back(fixture, RollbackOperation::GoneRegistration).await;
    }
}

// persist_terminal_notification_proof: ancestry locks, job UPDATE, descendant
// UPDATE, lineage UPDATE, provider SELECT, acknowledgement UPDATE (failed).
#[tokio::test]
async fn sqlite_terminal_proof_rolls_back_all_statements() {
    publisher_transaction_rolls_back(
        IngressFixture::sqlite().await,
        RollbackOperation::TerminalProof,
    )
    .await;
}

// Same job/descendant/lineage UPDATEs, then failed acknowledgement as SQLite.
#[tokio::test]
async fn postgres_terminal_proof_rolls_back_all_statements() {
    if let Some(fixture) = IngressFixture::postgres("publisher_proof_rollback").await {
        publisher_transaction_rolls_back(fixture, RollbackOperation::TerminalProof).await;
    }
}
