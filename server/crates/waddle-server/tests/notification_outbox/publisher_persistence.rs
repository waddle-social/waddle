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
