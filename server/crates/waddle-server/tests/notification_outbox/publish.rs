//! Outbox claim/publish lifecycle: claiming, retries, stale claims, coalescing after claim, pruning.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use crate::support::*;
use jid::Jid;
use waddle_server::db::Database;
use waddle_server::notification_outbox::*;
use waddle_xmpp::push::PushSubscriptionStore;

fn foreign_target() -> NotificationOutboxTarget {
    NotificationOutboxTarget::new(
        bare("push-provider.example.com"),
        PushServiceNodeName::new("web-node").expect("node"),
    )
}

async fn reclaim_stale_job(store: &TestStore) -> (NotificationOutboxJob, NotificationOutboxJob) {
    let stale_claim = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim")
        .into_iter()
        .next()
        .expect("claimed job");
    // Epoch-adjacent timestamp: older than any plausible claim
    // timeout, so the claim is unconditionally stale.
    let stale_claimed_at_ms = 1_i64;
    store
        .execute(
            "UPDATE notification_outbox SET claimed_at_ms = ? WHERE job_id = ?",
            waddle_server::db_params![stale_claimed_at_ms, stale_claim.job_id().as_str()],
        )
        .await
        .expect("make claim stale");
    let fresh_claim = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("reclaim")
        .into_iter()
        .next()
        .expect("reclaimed job");
    (stale_claim, fresh_claim)
}

struct FailingPushStore;

impl waddle_xmpp::push::PushSubscriptionStore for FailingPushStore {
    fn register(
        &self,
        _sub: waddle_xmpp::push::PushSubscription,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), waddle_xmpp::push::PushError>> + Send + '_>,
    > {
        Box::pin(std::future::ready(Ok(())))
    }

    fn remove(
        &self,
        _user_jid: &str,
        _service_jid: &str,
        _node: Option<&str>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), waddle_xmpp::push::PushError>> + Send + '_>,
    > {
        Box::pin(std::future::ready(Ok(())))
    }

    fn get_for_user(
        &self,
        _user_jid: &str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Vec<waddle_xmpp::push::PushSubscription>,
                        waddle_xmpp::push::PushError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(std::future::ready(Err(
            waddle_xmpp::push::PushError::StorageError("registration store unavailable".into()),
        )))
    }
}

#[tokio::test]
async fn claimed_outbox_job_builds_stable_xep0357_pubsub_item() {
    let store = store().await;
    let target = target();
    let candidate = candidate("archive-1");
    enqueue_jobs_for_test(&store, &candidate, &[target]).await;

    let jobs = store.claim_due_outbox_jobs(16).await.expect("claim");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status(), NotificationOutboxStatus::InProgress);
    let item = jobs[0].to_xep0357_pubsub_item();
    assert_eq!(item.id.as_deref(), Some(jobs[0].job_id().as_str()));
    let payload = item.payload.expect("payload");
    assert!(payload.is("notification", waddle_xmpp::xep::xep0357::NS_PUSH));
}

#[tokio::test]
async fn claim_due_outbox_jobs_fails_malformed_sender_set_before_publish() {
    let store = store().await;
    enqueue_jobs_for_test(&store, &candidate("archive-malformed-claim"), &[target()]).await;
    store
        .execute(
            "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
            waddle_server::db_params!["bob@example.com", "[]"],
        )
        .await
        .expect("make queued job malformed");

    let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

    assert!(claimed.is_empty());
    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
}

#[tokio::test]
async fn claim_due_outbox_jobs_fails_empty_sender_without_conversation_fallback() {
    let store = store().await;
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-empty-sender-claim"),
        &[target()],
    )
    .await;
    store
        .execute(
            "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
            waddle_server::db_params!["", "[]"],
        )
        .await
        .expect("make queued job sender provenance empty");

    let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

    assert!(claimed.is_empty());
    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
}

#[tokio::test]
async fn claim_due_outbox_jobs_fails_malformed_sender_jids_json_before_publish() {
    let store = store().await;
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-malformed-json-claim"),
        &[target()],
    )
    .await;
    store
        .execute(
            "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
            waddle_server::db_params!["bob@example.com/test-resource", "not-json"],
        )
        .await
        .expect("make queued job sender_jids malformed");

    let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

    assert!(claimed.is_empty());
    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
}

#[tokio::test]
async fn claim_due_outbox_jobs_fails_sender_set_missing_scalar_before_publish() {
    let store = store().await;
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-missing-scalar-claim"),
        &[target()],
    )
    .await;
    store
        .execute(
            "UPDATE notification_outbox SET sender_jids = ?",
            waddle_server::db_params!["[\"bob@example.com/laptop\"]"],
        )
        .await
        .expect("make queued job sender_jids omit scalar sender");

    let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

    assert!(claimed.is_empty());
    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
}

#[tokio::test]
async fn claim_due_outbox_jobs_fails_semantically_invalid_sender_jids_before_publish() {
    let store = store().await;
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-invalid-sender-jids-claim"),
        &[target()],
    )
    .await;
    store
        .execute(
            "UPDATE notification_outbox SET sender_jid = ?, sender_jids = ?",
            waddle_server::db_params![
                "bob@example.com/test-resource",
                "[\"carol@example.com/phone\"]"
            ],
        )
        .await
        .expect("make queued job sender_jids semantically invalid");

    let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

    assert!(claimed.is_empty());
    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
}

#[tokio::test]
async fn claim_due_outbox_jobs_fails_mismatched_scalar_sender_before_publish() {
    let store = store().await;
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-invalid-scalar-sender-claim"),
        &[target()],
    )
    .await;
    store
        .execute(
            "UPDATE notification_outbox SET sender_jid = ?",
            waddle_server::db_params!["carol@example.com/phone"],
        )
        .await
        .expect("make queued job scalar sender invalid");

    let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

    assert!(claimed.is_empty());
    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
}

#[tokio::test]
async fn claim_due_outbox_jobs_fails_malformed_context_before_publish() {
    let store = store().await;
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-malformed-context-claim"),
        &[target()],
    )
    .await;
    store
        .execute(
            "UPDATE notification_outbox SET context_xml = ?",
            waddle_server::db_params!["<context"],
        )
        .await
        .expect("make queued job context malformed");

    let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");

    assert!(claimed.is_empty());
    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
}

#[tokio::test]
async fn stale_in_progress_outbox_job_is_claimable_again() {
    let store = store().await;
    let target = target();
    let candidate = candidate("archive-1");
    enqueue_jobs_for_test(&store, &candidate, &[target]).await;

    let first_claim = store.claim_due_outbox_jobs(16).await.expect("first claim");
    assert_eq!(first_claim.len(), 1);
    let immediate_claim = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("immediate claim");
    assert!(immediate_claim.is_empty());

    // Epoch-adjacent timestamp: older than any plausible claim
    // timeout, so the claim is unconditionally stale.
    let stale_claimed_at_ms = 1_i64;
    store
        .execute(
            "UPDATE notification_outbox SET claimed_at_ms = ? WHERE job_id = ?",
            waddle_server::db_params![stale_claimed_at_ms, first_claim[0].job_id().as_str()],
        )
        .await
        .expect("make claim stale");

    let reclaimed = store.claim_due_outbox_jobs(16).await.expect("reclaim");
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].job_id(), first_claim[0].job_id());
    assert_eq!(reclaimed[0].status(), NotificationOutboxStatus::InProgress);
    assert_ne!(reclaimed[0].claim_token(), first_claim[0].claim_token());
}

#[tokio::test]
async fn stale_claim_does_not_enqueue_push_service_publish_job() {
    let store = store().await;
    let recipient = bare("alice@example.com");
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let push_service = waddle_server::push_service::DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("push-service").await.unwrap(),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push service");
    waddle_server::push_registrations::DatabasePushRegistrationStore::new(push_service.database())
        .await
        .expect("push registration schema");
    let push_node = push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    push_service
        .upsert_device(
            &recipient,
            waddle_server::push_service::PushDeviceRegistration::new(
                "web-1",
                push_node.node(),
                waddle_server::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", push_node.node(), None)
        .await
        .expect("first-party registration");
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    push_store
        .register(waddle_xmpp::push::PushSubscription {
            user_jid: recipient.to_string(),
            service_jid: "push.example.com".to_string(),
            node: Some(push_node.node().to_string()),
            publish_options: None,
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
        .expect("xep0357 registration");
    let target = NotificationOutboxTarget::new(
        bare("push.example.com"),
        PushServiceNodeName::new(push_node.node()).expect("push node target"),
    );
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-1"),
        std::slice::from_ref(&target),
    )
    .await;
    let (stale_claim, fresh_claim) = reclaim_stale_job(&store).await;
    let inbox = inbox_with_unread(&recipient, &bare("bob@example.com"), 1).await;

    let outcome = store
        .publish_claimed_job(
            &stale_claim,
            &push_service,
            &push_store,
            &inbox,
            &blocking,
            &bare("push.example.com"),
        )
        .await
        .expect("stale publish");

    assert!(matches!(
        outcome,
        NotificationOutboxPublishOutcome::RetryScheduled { .. }
    ));
    assert!(
        push_service
            .queued_publish_jobs()
            .await
            .expect("queued push jobs")
            .is_empty(),
        "stale claims must not enqueue durable Push Service publish jobs"
    );
    let pending = store.pending_outbox_jobs().await.expect("pending jobs");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].claim_token(), fresh_claim.claim_token());
}

#[tokio::test]
async fn new_candidate_after_claim_creates_fresh_queued_job() {
    let store = store().await;
    let target = target();
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-1"),
        std::slice::from_ref(&target),
    )
    .await;

    let claimed = store.claim_due_outbox_jobs(16).await.expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].message_count(), 1);
    enqueue_jobs_for_test(&store, &candidate("archive-2"), &[target]).await;

    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 2);
    assert!(jobs
        .iter()
        .any(|job| job.status() == NotificationOutboxStatus::InProgress));
    let queued = jobs
        .iter()
        .find(|job| job.status() == NotificationOutboxStatus::Queued)
        .expect("fresh queued job");
    assert_eq!(queued.message_count(), 1);
    assert_ne!(queued.job_id(), claimed[0].job_id());
}

#[tokio::test]
async fn coalesce_retry_creates_fresh_job_when_queued_job_is_claimed_after_select() {
    let store = store().await;
    let target = target();
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-race-1"),
        std::slice::from_ref(&target),
    )
    .await;
    store
        .db
        .guard()
        .await
        .expect("db guard")
        .execute(
            r#"
            CREATE TRIGGER simulate_notification_outbox_claim_race
            BEFORE UPDATE OF message_count ON notification_outbox
            WHEN OLD.status = 'queued'
            BEGIN
                UPDATE notification_outbox
                SET status = 'in-progress',
                    claimed_at_ms = OLD.updated_at_ms + 1,
                    claim_token = 'race-claim',
                    updated_at_ms = OLD.updated_at_ms + 1
                WHERE job_id = OLD.job_id;
                SELECT RAISE(IGNORE);
            END;
            "#,
            (),
        )
        .await
        .expect("install coalesce race trigger");

    enqueue_jobs_for_test(&store, &candidate("archive-race-2"), &[target]).await;

    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 2);
    assert!(jobs
        .iter()
        .any(|job| job.status() == NotificationOutboxStatus::InProgress
            && job.message_count() == 1
            && job.claim_token() == Some("race-claim")));
    let queued = jobs
        .iter()
        .find(|job| job.status() == NotificationOutboxStatus::Queued)
        .expect("fresh queued job from retry");
    assert_eq!(queued.message_count(), 1);
    assert_eq!(
        queued.sender_jids(),
        &["bob@example.com/test-resource"
            .parse::<Jid>()
            .expect("sender resource")]
    );
    let claimed = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim replacement job");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].status(), NotificationOutboxStatus::InProgress);
    assert_eq!(claimed[0].message_count(), 1);
    assert_ne!(claimed[0].claim_token(), Some("race-claim"));
    assert!(store
        .pending_candidates(16)
        .await
        .expect("pending candidates")
        .is_empty());
}

#[tokio::test]
async fn coalescing_new_candidate_clears_retry_backoff() {
    let store = store().await;
    let target = target();
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-1"),
        std::slice::from_ref(&target),
    )
    .await;
    let claimed = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim")
        .into_iter()
        .next()
        .expect("claimed job");
    assert_eq!(
        store
            .schedule_retry_or_fail(&claimed, "temporary failure".to_string())
            .await
            .expect("schedule retry"),
        Some(1)
    );
    assert!(
        store
            .claim_due_outbox_jobs(16)
            .await
            .expect("backoff claim")
            .is_empty(),
        "retry backoff should hide the job until new work arrives"
    );

    enqueue_jobs_for_test(&store, &candidate("archive-2"), &[target]).await;

    let reclaimed = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim after coalesce");
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].message_count(), 2);
    assert_eq!(reclaimed[0].attempt_count(), 1);
}

#[tokio::test]
async fn publish_rejects_non_first_party_outbox_target() {
    let store = store().await;
    enqueue_jobs_for_test(&store, &candidate("archive-1"), &[foreign_target()]).await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let push_service = waddle_server::push_service::DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("push-service").await.unwrap(),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push service");
    let push_store = waddle_server::push_registrations::DatabasePushRegistrationStore::new(
        Database::in_memory("push-regs").await.unwrap(),
    )
    .await
    .expect("push registrations");
    let inbox = inbox_with_unread(&bare("alice@example.com"), &bare("bob@example.com"), 1).await;

    let outcomes = store
        .drain_due_outbox_jobs(
            &push_service,
            &push_store,
            &inbox,
            &blocking,
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain outbox");

    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes[0],
        NotificationOutboxPublishOutcome::Failed { .. }
    ));
    assert!(
        push_service
            .queued_publish_jobs()
            .await
            .expect("queued push jobs")
            .is_empty(),
        "foreign outbox target must not enqueue a first-party Push Service job"
    );
}

#[tokio::test]
async fn push_registration_lookup_error_retries_each_claimed_outbox_job() {
    let store = store().await;
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-1"),
        &[target_named("web-node-1"), target_named("web-node-2")],
    )
    .await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let push_service = waddle_server::push_service::DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("push-service").await.unwrap(),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push service");
    let inbox = inbox_with_unread(&bare("alice@example.com"), &bare("bob@example.com"), 1).await;

    let outcomes = store
        .drain_due_outbox_jobs(
            &push_service,
            &FailingPushStore,
            &inbox,
            &blocking,
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain");

    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|outcome| matches!(
        outcome,
        NotificationOutboxPublishOutcome::RetryScheduled { .. }
    )));
    let pending = store.pending_outbox_jobs().await.expect("pending");
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().all(|job| {
        job.status() == NotificationOutboxStatus::Queued && job.attempt_count() == 1
    }));
}

#[tokio::test]
async fn prune_completed_removes_only_finished_jobs_and_outboxed_candidates() {
    let store = store().await;
    enqueue_jobs_for_test(&store, &candidate("archive-old"), &[target()]).await;
    let old_job = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim old job")
        .into_iter()
        .next()
        .expect("old job");
    assert!(store.mark_job_published(&old_job).await.expect("published"));

    enqueue_jobs_for_test(&store, &candidate("archive-live"), &[target()]).await;
    let cutoff_ms = waddle_server::time::now_ms().saturating_sub(1_000);
    let old_ms = cutoff_ms.saturating_sub(1);
    store
        .execute(
            "UPDATE notification_candidates SET outboxed_at_ms = ? WHERE stanza_id = ?",
            waddle_server::db_params![old_ms, "archive-old"],
        )
        .await
        .expect("age old candidate");
    store
        .execute(
            "UPDATE notification_outbox SET updated_at_ms = ? WHERE job_id = ?",
            waddle_server::db_params![old_ms, old_job.job_id().as_str()],
        )
        .await
        .expect("age old job");

    let pruned = store
        .prune_completed_before(cutoff_ms, 100)
        .await
        .expect("prune");

    assert_eq!(pruned.candidates_deleted, 1);
    assert_eq!(pruned.jobs_deleted, 1);
    let mut candidate_count = store
        .query("SELECT COUNT(*) FROM notification_candidates", ())
        .await
        .expect("candidate count query");
    let candidate_row = candidate_count
        .next()
        .await
        .expect("candidate count row")
        .expect("candidate count");
    assert_eq!(candidate_row.get::<i64>(0).expect("candidate count"), 1);
    let pending_jobs = store.pending_outbox_jobs().await.expect("pending jobs");
    assert_eq!(pending_jobs.len(), 1);
    assert_eq!(pending_jobs[0].status(), NotificationOutboxStatus::Queued);
}

#[tokio::test]
async fn prune_completed_deletes_outboxed_candidates_in_ordered_batches() {
    let store = store().await;
    enqueue_jobs_for_test(&store, &candidate("archive-oldest"), &[target()]).await;
    enqueue_jobs_for_test(&store, &candidate("archive-older"), &[target()]).await;
    enqueue_jobs_for_test(&store, &candidate("archive-live"), &[target()]).await;
    let cutoff_ms = waddle_server::time::now_ms().saturating_sub(1_000);
    let oldest_ms = cutoff_ms.saturating_sub(2);
    let older_ms = cutoff_ms.saturating_sub(1);
    let live_ms = cutoff_ms.saturating_add(1);
    store
        .execute(
            "UPDATE notification_candidates SET outboxed_at_ms = ? WHERE stanza_id = ?",
            waddle_server::db_params![oldest_ms, "archive-oldest"],
        )
        .await
        .expect("age oldest candidate");
    store
        .execute(
            "UPDATE notification_candidates SET outboxed_at_ms = ? WHERE stanza_id = ?",
            waddle_server::db_params![older_ms, "archive-older"],
        )
        .await
        .expect("age older candidate");
    store
        .execute(
            "UPDATE notification_candidates SET outboxed_at_ms = ? WHERE stanza_id = ?",
            waddle_server::db_params![live_ms, "archive-live"],
        )
        .await
        .expect("keep live candidate");

    let pruned = store
        .prune_completed_before(cutoff_ms, 1)
        .await
        .expect("prune");

    assert_eq!(pruned.candidates_deleted, 1);
    assert_eq!(pruned.jobs_deleted, 0);
    let mut rows = store
        .query(
            "SELECT stanza_id FROM notification_candidates ORDER BY outboxed_at_ms ASC",
            (),
        )
        .await
        .expect("candidate query");
    let mut remaining = Vec::new();
    while let Some(row) = rows.next().await.expect("candidate row") {
        remaining.push(row.get::<String>(0).expect("stanza id"));
    }
    assert_eq!(
        remaining,
        vec!["archive-older".to_string(), "archive-live".to_string()]
    );
}
