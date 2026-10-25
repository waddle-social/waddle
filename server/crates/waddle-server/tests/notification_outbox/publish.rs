//! Outbox claim/publish lifecycle: claiming, retries, stale claims, coalescing after claim, pruning.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use crate::support::{
    bare, candidate, enqueue_jobs_for_test, failed_outbox_jobs_count, groupchat_candidate_for,
    inbox_with_unread, store, target, target_named, FailingBlockingStorage, TestStore,
};
use jid::Jid;
use waddle_server::db::Database;
use waddle_server::notification_outbox::{
    NotificationClass, NotificationOutboxJob, NotificationOutboxPublishOutcome,
    NotificationOutboxStatus, NotificationOutboxStore, NotificationOutboxTarget,
    PushServiceNodeName, WADDLE_PUSH_CONTEXT_NS,
};
use waddle_xmpp::push::PushSubscriptionStore;

fn foreign_target() -> NotificationOutboxTarget {
    NotificationOutboxTarget::new(
        bare("push-provider.example.com"),
        PushServiceNodeName::try_new("web-node").expect("node"),
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
    expire_claim_into_backoff(store, &stale_claim, 3_750, 6_250).await;
    store
        .execute(
            "UPDATE notification_outbox SET next_attempt_at_ms = 1 WHERE job_id = ?",
            waddle_server::db_params![stale_claim.job_id().as_str()],
        )
        .await
        .expect("advance retry deadline");
    let fresh_claim = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("reclaim")
        .into_iter()
        .next()
        .expect("reclaimed job");
    assert_ne!(fresh_claim.claim_token(), stale_claim.claim_token());
    assert_eq!(fresh_claim.attempt_count(), stale_claim.attempt_count() + 1);
    (stale_claim, fresh_claim)
}

async fn expire_claim_into_backoff(
    store: &TestStore,
    job: &NotificationOutboxJob,
    minimum_delay_ms: i64,
    maximum_delay_ms: i64,
) {
    // Epoch-adjacent timestamp is unconditionally older than the claim timeout.
    store
        .execute(
            "UPDATE notification_outbox SET claimed_at_ms = 1 WHERE job_id = ?",
            waddle_server::db_params![job.job_id().as_str()],
        )
        .await
        .expect("expire claim");
    let before = waddle_server::time::now_ms();
    assert!(store
        .claim_due_outbox_jobs(16)
        .await
        .expect("schedule expired claim retry")
        .is_empty());
    let after = waddle_server::time::now_ms();
    let mut rows = store
        .query(
            "SELECT status, attempt_count, next_attempt_at_ms, claim_token, claimed_at_ms FROM notification_outbox WHERE job_id = ?",
            waddle_server::db_params![job.job_id().as_str()],
        )
        .await
        .expect("persisted retry");
    let row = rows.next().await.expect("row").expect("job");
    assert_eq!(row.get::<String>(0).expect("status"), "queued");
    assert_eq!(
        row.get::<i64>(1).expect("attempts"),
        job.attempt_count() + 1
    );
    let deadline = row.get::<i64>(2).expect("deadline");
    assert!(deadline >= before + minimum_delay_ms && deadline <= after + maximum_delay_ms);
    assert!(row.get::<Option<String>>(3).expect("token").is_none());
    assert!(row.get::<Option<i64>>(4).expect("claim time").is_none());
    drop(rows);
    assert!(store
        .claim_due_outbox_jobs(16)
        .await
        .expect("backoff prevents early claim")
        .is_empty());
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
    // #779: the context carries the originating message's XEP-0359
    // stanza-id (the SW dedup key) while the pubsub item id above
    // stays the job id (coalesce/idempotency stability).
    let context = payload
        .children()
        .find(|child| child.is("context", WADDLE_PUSH_CONTEXT_NS))
        .expect("waddle context");
    assert_eq!(context.attr("stanza-id"), Some("archive-1"));
    // Item construction proves stable protocol identity only: it performs
    // no durable Push Service acceptance and no provider request.
    let recipient = bare("alice@example.com");
    let (push_service, _, target) = first_party_push_setup(&recipient).await;
    assert!(push_service
        .queued_publish_jobs()
        .await
        .expect("queue")
        .is_empty());
    assert!(push_service
        .delivery_attempts_for_node(target.node().as_str())
        .await
        .expect("provider attempts")
        .is_empty());
    assert_eq!(
        store.pending_outbox_jobs().await.expect("outbox")[0].status(),
        NotificationOutboxStatus::InProgress
    );
}

#[tokio::test]
async fn lost_acceptance_reply_replays_frozen_payload_without_coalescing_new_work() {
    let store = store().await;
    let recipient = bare("alice@example.com");
    let (push_service, push_store, target) = first_party_push_setup(&recipient).await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-first"),
        std::slice::from_ref(&target),
    )
    .await;
    let claimed = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim")
        .remove(0);
    let inbox = inbox_with_unread(&recipient, &bare("bob@example.com"), 2).await;
    store
        .publish_claimed_job(
            &claimed,
            &push_service,
            &push_store,
            &inbox,
            &blocking,
            &bare("push.example.com"),
        )
        .await
        .expect("durable queue acceptance");
    // The downstream accepted, but the upstream failed to observe the reply.
    store
        .execute(
            "UPDATE notification_outbox SET status = ?, published_at_ms = NULL WHERE job_id = ?",
            waddle_server::db_params!["queued", claimed.job_id().as_str()],
        )
        .await
        .expect("lost reply");
    enqueue_jobs_for_test(&store, &candidate("archive-second"), &[target]).await;
    let mut jobs = store.claim_due_outbox_jobs(16).await.expect("reclaim");
    assert_eq!(
        jobs.len(),
        2,
        "new work must not mutate the already approved delivery"
    );
    let retry = jobs.remove(
        jobs.iter()
            .position(|job| job.job_id() == claimed.job_id())
            .expect("same identity"),
    );
    assert_eq!(retry.message_count(), 1);
    let original = retry.to_xep0357_pubsub_item_with_count(99);
    let read_inbox = inbox_with_read_entry(
        &recipient,
        &bare("bob@example.com"),
        waddle_xmpp::inbox::ConversationKind::Direct,
    )
    .await;
    assert!(
        matches!(
            store
                .publish_claimed_job(
                    &retry,
                    &push_service,
                    &push_store,
                    &read_inbox,
                    &blocking,
                    &bare("push.example.com")
                )
                .await
                .expect("retry"),
            NotificationOutboxPublishOutcome::Published { .. }
        ),
        "approved payload must remain replayable after unread changes"
    );
    let accepted = push_service
        .queued_publish_jobs()
        .await
        .expect("accepted jobs");
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].item_id(), claimed.job_id().as_str());
    let db = push_service.database();
    let conn = db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT payload_xml FROM push_publish_jobs WHERE item_id = ?",
            waddle_server::db_params![claimed.job_id().as_str()],
        )
        .await
        .expect("payload");
    let payload: String = rows
        .next()
        .await
        .expect("row")
        .expect("accepted")
        .get(0)
        .expect("payload");
    assert_eq!(
        payload,
        String::from(original.payload.as_ref().expect("frozen payload"))
    );
    assert!(
        push_service
            .delivery_attempts_for_node(retry.node().as_str())
            .await
            .expect("provider attempts")
            .is_empty(),
        "durable queue acceptance is not provider acceptance"
    );
}

// #779: coalescing must keep the context's stanza-id pointing at the
// LATEST message merged into the job — that is the id the foreground
// tab most recently posted to the SW dedup store.
#[tokio::test]
async fn coalesced_job_context_names_latest_stanza_id() {
    let store = store().await;
    let target = target();
    enqueue_jobs_for_test(
        &store,
        &candidate("archive-first"),
        std::slice::from_ref(&target),
    )
    .await;
    enqueue_jobs_for_test(&store, &candidate("archive-second"), &[target]).await;

    let jobs = store.claim_due_outbox_jobs(16).await.expect("claim");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].message_count(), 2, "coalesced into one job");
    let payload = jobs[0].to_xep0357_pubsub_item().payload.expect("payload");
    let context = payload
        .children()
        .find(|child| child.is("context", WADDLE_PUSH_CONTEXT_NS))
        .expect("waddle context");
    assert_eq!(
        context.attr("stanza-id"),
        Some("archive-second"),
        "merge must overwrite the context with the newest candidate's stanza-id"
    );
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

    expire_claim_into_backoff(&store, &first_claim[0], 3_750, 6_250).await;
    store
        .execute(
            "UPDATE notification_outbox SET next_attempt_at_ms = 1 WHERE job_id = ?",
            waddle_server::db_params![first_claim[0].job_id().as_str()],
        )
        .await
        .expect("advance retry deadline");

    let reclaimed = store.claim_due_outbox_jobs(16).await.expect("reclaim");
    assert_eq!(reclaimed.len(), 1);
    assert_eq!(reclaimed[0].job_id(), first_claim[0].job_id());
    assert_eq!(reclaimed[0].status(), NotificationOutboxStatus::InProgress);
    assert_eq!(reclaimed[0].attempt_count(), 1);
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
    waddle_server::push_registrations::DatabasePushRegistrationStore::try_new(
        push_service.database(),
    )
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
        PushServiceNodeName::try_new(push_node.node()).expect("push node target"),
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

/// Full first-party Push Service setup (node, device, first-party +
/// XEP-0357 registrations) for tests that drive `publish_claimed_job`
/// end-to-end. Returns the push service store, the subscription
/// store, and an outbox target aimed at the provisioned node.
async fn first_party_push_setup(
    recipient: &jid::BareJid,
) -> (
    waddle_server::push_service::DatabasePushServiceStore,
    waddle_xmpp::push::InMemoryPushStore,
    NotificationOutboxTarget,
) {
    let push_service = waddle_server::push_service::DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("push-service").await.unwrap(),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push service");
    waddle_server::push_registrations::DatabasePushRegistrationStore::try_new(
        push_service.database(),
    )
    .await
    .expect("push registration schema");
    let push_node = push_service
        .ensure_node(recipient, "web")
        .await
        .expect("push node");
    push_service
        .upsert_device(
            recipient,
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
        .register_first_party_node_for_owner(recipient, "push.example.com", push_node.node(), None)
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
        PushServiceNodeName::try_new(push_node.node()).expect("push node target"),
    );
    (push_service, push_store, target)
}

/// Inbox with one entry for `conversation` whose unread count is 0 —
/// the positive "recipient read it" signal (#1126 suppression
/// requires the entry to EXIST; absence means "no information").
async fn inbox_with_read_entry(
    recipient: &jid::BareJid,
    conversation: &jid::BareJid,
    kind: waddle_xmpp::inbox::ConversationKind,
) -> waddle_xmpp::inbox::storage::InMemoryInboxStorage {
    use waddle_xmpp::inbox::storage::InboxStorage as _;
    let inbox = waddle_xmpp::inbox::storage::InMemoryInboxStorage::new();
    inbox
        .upsert(
            recipient,
            waddle_xmpp::inbox::InboxEntry::new(
                conversation.clone(),
                kind,
                "archive-read".to_string(),
                1,
            ),
            false,
        )
        .await
        .expect("upsert read entry");
    inbox
}

// #1126: a user who reconnects and reads the message inside the
// notification window must not get an OS push for it. At publish
// time the conversation's inbox entry exists with unread 0 →
// suppress, terminally.
#[tokio::test]
async fn unread_zero_job_is_suppressed_not_published() {
    let store = store().await;
    let recipient = bare("alice@example.com");
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let (push_service, push_store, target) = first_party_push_setup(&recipient).await;
    enqueue_jobs_for_test(&store, &candidate("archive-1"), &[target]).await;
    let claimed = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim")
        .into_iter()
        .next()
        .expect("claimed job");
    // Recipient read everything before the outbox drained: the entry
    // exists and its unread count was cleared to 0.
    let inbox = inbox_with_read_entry(
        &recipient,
        &bare("bob@example.com"),
        waddle_xmpp::inbox::ConversationKind::Direct,
    )
    .await;

    let outcome = store
        .publish_claimed_job(
            &claimed,
            &push_service,
            &push_store,
            &inbox,
            &blocking,
            &bare("push.example.com"),
        )
        .await
        .expect("publish");

    assert!(
        matches!(outcome, NotificationOutboxPublishOutcome::Suppressed { .. }),
        "#1126: unread-count 0 at publish time must suppress, got {outcome:?}"
    );
    assert!(
        push_service
            .queued_publish_jobs()
            .await
            .expect("queued push jobs")
            .is_empty(),
        "a suppressed job must not enqueue a durable Push Service publish job"
    );
    assert!(
        store
            .pending_outbox_jobs()
            .await
            .expect("pending jobs")
            .is_empty(),
        "suppression is terminal — the job must not stay queued or retry"
    );
}

// #1126 safety valve: an ABSENT inbox entry is "no information", not
// "read". The inbox projection is written on a separate path whose
// failures are swallowed, so suppressing on absence would turn a
// transient projection failure into total notification loss. Absent
// entries keep the pre-#1126 behavior: publish (with count 0).
#[tokio::test]
async fn missing_inbox_entry_still_publishes() {
    let store = store().await;
    let recipient = bare("alice@example.com");
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let (push_service, push_store, target) = first_party_push_setup(&recipient).await;
    enqueue_jobs_for_test(&store, &candidate("archive-1"), &[target]).await;
    let claimed = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim")
        .into_iter()
        .next()
        .expect("claimed job");
    // No inbox entry at all for the conversation (projection lagged
    // or its write failed).
    let inbox = waddle_xmpp::inbox::storage::InMemoryInboxStorage::new();

    let outcome = store
        .publish_claimed_job(
            &claimed,
            &push_service,
            &push_store,
            &inbox,
            &blocking,
            &bare("push.example.com"),
        )
        .await
        .expect("publish");

    assert!(
        matches!(outcome, NotificationOutboxPublishOutcome::Published { .. }),
        "an absent inbox entry must NOT suppress the push, got {outcome:?}"
    );
    assert_eq!(
        push_service
            .queued_publish_jobs()
            .await
            .expect("queued push jobs")
            .len(),
        1
    );
}

// #1126 groupchat coverage: mention classes resolve their unread via
// the MUC room's inbox entry — a read room entry (unread 0) must
// suppress just like the DM path.
#[tokio::test]
async fn read_channel_mention_job_is_suppressed() {
    let store = store().await;
    let recipient = bare("alice@example.com");
    let room = bare("room@conference.example.com");
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let (push_service, push_store, target) = first_party_push_setup(&recipient).await;
    let candidate = groupchat_candidate_for(
        &recipient,
        &room,
        "room@conference.example.com/bob".parse().expect("occupant"),
        "archive-mention-1",
        NotificationClass::PersonalMention,
    );
    enqueue_jobs_for_test(&store, &candidate, &[target]).await;
    let claimed = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim")
        .into_iter()
        .next()
        .expect("claimed job");
    let inbox = inbox_with_read_entry(
        &recipient,
        &room,
        waddle_xmpp::inbox::ConversationKind::MucRoom,
    )
    .await;

    let outcome = store
        .publish_claimed_job(
            &claimed,
            &push_service,
            &push_store,
            &inbox,
            &blocking,
            &bare("push.example.com"),
        )
        .await
        .expect("publish");

    assert!(
        matches!(outcome, NotificationOutboxPublishOutcome::Suppressed { .. }),
        "#1126: a read MUC room entry must suppress a mention push, got {outcome:?}"
    );
    assert!(push_service
        .queued_publish_jobs()
        .await
        .expect("queued push jobs")
        .is_empty());
}

// #1126 companion: a genuinely offline recipient (unread >= 1) still
// gets the push — suppression only applies to already-read messages.
#[tokio::test]
async fn unread_nonzero_job_still_publishes() {
    let store = store().await;
    let recipient = bare("alice@example.com");
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let (push_service, push_store, target) = first_party_push_setup(&recipient).await;
    enqueue_jobs_for_test(&store, &candidate("archive-1"), &[target]).await;
    let claimed = store
        .claim_due_outbox_jobs(16)
        .await
        .expect("claim")
        .into_iter()
        .next()
        .expect("claimed job");
    let inbox = inbox_with_unread(&recipient, &bare("bob@example.com"), 2).await;

    let outcome = store
        .publish_claimed_job(
            &claimed,
            &push_service,
            &push_store,
            &inbox,
            &blocking,
            &bare("push.example.com"),
        )
        .await
        .expect("publish");

    assert!(
        matches!(outcome, NotificationOutboxPublishOutcome::Published { .. }),
        "legitimate offline pushes must still fire, got {outcome:?}"
    );
    assert_eq!(
        push_service
            .queued_publish_jobs()
            .await
            .expect("queued push jobs")
            .len(),
        1,
        "an unread conversation must enqueue the durable Push Service publish job"
    );
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
    let telemetry = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    enqueue_jobs_for_test(&store, &candidate("archive-1"), &[foreign_target()]).await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let push_service = waddle_server::push_service::DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("push-service").await.unwrap(),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push service");
    let push_store = waddle_server::push_registrations::DatabasePushRegistrationStore::try_new(
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
    assert_eq!(
        telemetry.counter_sum("xmpp.push.disposition", &[("disposition", "dead_letter")]),
        Some(1)
    );
    assert_eq!(
        telemetry.counter_sum("xmpp.push.outbox_dead_lettered", &[]),
        Some(1)
    );

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
    let telemetry = waddle_xmpp::telemetry::test_support::acquire().await;
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

    assert_eq!(
        telemetry.counter_sum(
            "xmpp.push.outbox_retry_scheduled",
            &[("reason", "registration_lookup")],
        ),
        Some(2),
    );
    assert_eq!(
        telemetry
            .counter_sum("xmpp.push.outbox_retry_scheduled", &[("reason", "unknown")])
            .unwrap_or(0),
        0,
    );
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

    store
        .execute(
            "UPDATE notification_outbox_lineage SET settled_at_ms = ? WHERE job_id = ?",
            waddle_server::db_params![
                waddle_server::time::now_ms().saturating_sub(9 * 24 * 60 * 60 * 1_000),
                old_job.job_id().as_str()
            ],
        )
        .await
        .expect("provider scheduling disposition aged beyond the tail");
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

    store
        .execute(
            "UPDATE notification_outbox_lineage SET settled_at_ms = ?",
            waddle_server::db_params![
                waddle_server::time::now_ms().saturating_sub(9 * 24 * 60 * 60 * 1_000)
            ],
        )
        .await
        .expect("terminal scheduling dispositions");
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

#[tokio::test]
async fn prune_completed_reaps_policy_retry_dead_lettered_candidate() {
    let store = store().await;
    let candidate = candidate("archive-policy-retries-exhausted");
    store
        .insert_candidate(&candidate)
        .await
        .expect("insert candidate");
    let cutoff_ms = waddle_server::time::now_ms().saturating_sub(1_000);
    let old_ms = cutoff_ms.saturating_sub(1);
    store
        .execute(
            r#"
            UPDATE notification_candidates
            SET outboxed_at_ms = ?,
                suppressed_reason = ?
            WHERE stanza_id = ?
            "#,
            waddle_server::db_params![
                old_ms,
                "policy_retries_exhausted",
                "archive-policy-retries-exhausted",
            ],
        )
        .await
        .expect("mark candidate dead-lettered");

    let pruned = store
        .prune_completed_before(cutoff_ms, 100)
        .await
        .expect("prune");

    assert_eq!(pruned.candidates_deleted, 1);
    assert_eq!(pruned.jobs_deleted, 0);
    let mut rows = store
        .query(
            "SELECT COUNT(*) FROM notification_candidates WHERE stanza_id = ?",
            waddle_server::db_params!["archive-policy-retries-exhausted"],
        )
        .await
        .expect("candidate count query");
    let row = rows
        .next()
        .await
        .expect("candidate count row")
        .expect("candidate count");
    assert_eq!(row.get::<i64>(0).expect("candidate count"), 0);
}

#[tokio::test]
async fn repeated_expired_outbox_claims_park_uncertain_work_at_the_budget() {
    let telemetry = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    enqueue_jobs_for_test(&store, &candidate("expired-budget"), &[target()]).await;
    let mut job = store
        .claim_due_outbox_jobs(1)
        .await
        .expect("initial claim")
        .remove(0);
    let payload = job.to_xep0357_pubsub_item().payload.expect("payload");
    store
        .execute(
            "UPDATE notification_outbox SET queue_acceptance_may_exist = 1, approved_payload_xml = ? WHERE job_id = ?",
            waddle_server::db_params![String::from(&payload), job.job_id().as_str()],
        )
        .await
        .expect("unknown queue acceptance");
    for (index, (minimum_delay_ms, maximum_delay_ms)) in [
        (225_000, 375_000),
        (450_000, 750_000),
        (900_000, 1_500_000),
        (1_800_000, 3_000_000),
    ]
    .into_iter()
    .enumerate()
    {
        expire_claim_into_backoff(&store, &job, minimum_delay_ms, maximum_delay_ms).await;
        store
            .execute(
                "UPDATE notification_outbox SET next_attempt_at_ms = 1 WHERE job_id = ?",
                waddle_server::db_params![job.job_id().as_str()],
            )
            .await
            .expect("advance retry deadline");
        let mut claimed = store.claim_due_outbox_jobs(1).await.expect("recover claim");
        assert_eq!(claimed.len(), 1);
        assert_ne!(claimed[0].claim_token(), job.claim_token());
        job = claimed.remove(0);
        assert_eq!(job.attempt_count(), index as i64 + 1);
    }
    store
        .execute(
            "UPDATE notification_outbox SET claimed_at_ms = 1 WHERE job_id = ?",
            waddle_server::db_params![job.job_id().as_str()],
        )
        .await
        .expect("expire final claim");
    assert!(store
        .claim_due_outbox_jobs(1)
        .await
        .expect("exhausted recovery")
        .is_empty());
    for _ in 0..3 {
        assert!(store
            .claim_due_outbox_jobs(1)
            .await
            .expect("parked recovery")
            .is_empty());
    }
    assert_eq!(failed_outbox_jobs_count(&store).await, 1);
    let mut rows = store
        .query(
            "SELECT attempt_count, next_attempt_at_ms, claim_token, queue_acceptance_may_exist, approved_payload_xml, (SELECT COUNT(*) FROM notification_outbox_lineage WHERE job_id = notification_outbox.job_id AND settled_at_ms IS NULL) FROM notification_outbox WHERE job_id = ?",
            waddle_server::db_params![job.job_id().as_str()],
        )
        .await
        .expect("parked custody");
    let row = rows.next().await.expect("row").expect("parked job");
    assert_eq!(row.get::<i64>(0).expect("attempts"), 5);
    assert!(row.get::<Option<i64>>(1).expect("deadline").is_none());
    assert!(row.get::<Option<String>>(2).expect("token").is_none());
    assert_eq!(row.get::<i64>(3).expect("uncertainty"), 1);
    assert_eq!(
        row.get::<String>(4).expect("approved payload"),
        String::from(&payload)
    );
    assert_eq!(row.get::<i64>(5).expect("unsettled lineage"), 1);
    drop(rows);
    assert_eq!(
        store
            .prune_completed_before(i64::MAX, 16)
            .await
            .expect("prune")
            .jobs_deleted,
        0
    );
    assert_eq!(
        telemetry.counter_sum("xmpp.push.disposition", &[("disposition", "parked")]),
        Some(1)
    );
    assert_eq!(
        telemetry
            .counter_sum("xmpp.push.outbox_dead_lettered", &[])
            .unwrap_or(0),
        0
    );
}

#[tokio::test]
async fn persistent_outbox_policy_failures_stop_at_the_budget() {
    let telemetry = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let recipient = bare("alice@example.com");
    let (push_service, push_store, target) = first_party_push_setup(&recipient).await;
    enqueue_jobs_for_test(&store, &candidate("policy-budget"), &[target]).await;
    let inbox = inbox_with_unread(&recipient, &bare("bob@example.com"), 1).await;
    for attempt in 1..=5 {
        let outcome = store
            .drain_due_outbox_jobs(
                &push_service,
                &push_store,
                &inbox,
                &FailingBlockingStorage,
                &bare("push.example.com"),
                1,
            )
            .await
            .expect("policy failure");
        assert_eq!(outcome.len(), 1);
        assert_eq!(
            matches!(outcome[0], NotificationOutboxPublishOutcome::Failed { .. }),
            attempt == 5
        );
        store
            .execute(
                "UPDATE notification_outbox SET next_attempt_at_ms = 1 WHERE status = 'queued'",
                (),
            )
            .await
            .expect("advance retry");
    }
    assert!(store
        .drain_due_outbox_jobs(
            &push_service,
            &push_store,
            &inbox,
            &FailingBlockingStorage,
            &bare("push.example.com"),
            1
        )
        .await
        .expect("exhausted policy")
        .is_empty());
    assert!(push_service
        .queued_publish_jobs()
        .await
        .expect("provider queue")
        .is_empty());
    assert_eq!(
        telemetry.counter_sum(
            "xmpp.push.outbox_retry_scheduled",
            &[("reason", "policy_lookup")]
        ),
        Some(4)
    );
    assert_eq!(
        telemetry.counter_sum("xmpp.push.disposition", &[("disposition", "dead_letter")]),
        Some(1)
    );
}

#[tokio::test]
async fn parked_outbox_reconciles_revocation_from_a_separate_push_database() {
    let telemetry = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let recipient = bare("alice@example.com");
    let (push_service, push_store, target) = first_party_push_setup(&recipient).await;
    enqueue_jobs_for_test(
        &store,
        &candidate("parked-revocation"),
        std::slice::from_ref(&target),
    )
    .await;
    let job = store
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let inbox = inbox_with_unread(&recipient, &bare("bob@example.com"), 1).await;
    store
        .publish_claimed_job(
            &job,
            &push_service,
            &push_store,
            &inbox,
            &blocking,
            &bare("push.example.com"),
        )
        .await
        .expect("queue acceptance");
    // The queue commit succeeded but its reply never closed the outbox owner.
    // Repeated uncertain retries have now exhausted that owner's budget.
    store.execute("UPDATE notification_outbox SET status = 'failed', attempt_count = 5, queue_acceptance_may_exist = 1, published_at_ms = NULL WHERE job_id = ?", waddle_server::db_params![job.job_id().as_str()]).await.expect("park exhausted owner");
    let db = push_service.database();
    db.guard()
        .await
        .expect("provider db")
        .execute(
            "UPDATE push_publish_jobs SET upstream_completed = 0 WHERE ancestry_job_id = ?",
            waddle_server::db_params![job.job_id().as_str()],
        )
        .await
        .expect("lost completion acknowledgment");
    assert!(store
        .drain_due_outbox_jobs(
            &push_service,
            &push_store,
            &inbox,
            &blocking,
            &bare("push.example.com"),
            1
        )
        .await
        .expect("parked drain")
        .is_empty());
    assert_eq!(
        push_service
            .queued_publish_jobs()
            .await
            .expect("queue")
            .len(),
        1
    );
    push_service
        .remove_registered_nodes_for_owner(
            &recipient,
            "push.example.com",
            Some(target.node().as_str()),
        )
        .await
        .expect("authoritative revocation");
    // Endpoint revocation and upstream outbox reconciliation are distinct
    // lifecycle transitions. Measure this owner's reconciliation separately.
    let revoked_before_reconciliation = telemetry
        .counter_sum(
            "xmpp.push.disposition",
            &[("disposition", "registration_revoked")],
        )
        .unwrap_or(0);
    let outcomes = store
        .drain_due_outbox_jobs(
            &push_service,
            &push_store,
            &inbox,
            &blocking,
            &bare("push.example.com"),
            1,
        )
        .await
        .expect("terminal-only reconciliation");
    assert!(matches!(
        outcomes.as_slice(),
        [NotificationOutboxPublishOutcome::Failed { .. }]
    ));
    let mut rows = store
        .query(
            "SELECT queue_acceptance_may_exist FROM notification_outbox WHERE job_id = ?",
            waddle_server::db_params![job.job_id().as_str()],
        )
        .await
        .expect("outbox disposition");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("retained outbox")
            .get::<i64>(0)
            .expect("certainty"),
        0
    );
    let mut rows = db
        .guard()
        .await
        .expect("db")
        .query(
            "SELECT upstream_completed FROM push_publish_jobs WHERE ancestry_job_id = ?",
            waddle_server::db_params![job.job_id().as_str()],
        )
        .await
        .expect("ack");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("retained provider proof")
            .get::<i64>(0)
            .expect("completed"),
        1
    );
    assert_eq!(
        telemetry
            .counter_sum(
                "xmpp.push.disposition",
                &[("disposition", "registration_revoked")]
            )
            .expect("reconciliation disposition")
            - revoked_before_reconciliation,
        1,
        "reconciling this outbox owner records one additional transition",
    );
}

#[tokio::test]
async fn bounded_parked_probes_rotate_past_unresolved_owners_across_restarts() {
    let store = store().await;
    let recipient = bare("alice@example.com");
    let (push, registrations, target) = first_party_push_setup(&recipient).await;
    enqueue_jobs_for_test(
        &store,
        &candidate("later-terminal-proof"),
        std::slice::from_ref(&target),
    )
    .await;
    let terminal = store
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    let inbox = inbox_with_unread(&recipient, &bare("bob@example.com"), 1).await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    store
        .publish_claimed_job(
            &terminal,
            &push,
            &registrations,
            &inbox,
            &blocking,
            &bare("push.example.com"),
        )
        .await
        .expect("acceptance");
    store.execute("UPDATE notification_outbox SET status = 'failed', queue_acceptance_may_exist = 1, attempt_count = 5, next_attempt_at_ms = 2, published_at_ms = NULL WHERE job_id = ?", waddle_server::db_params![terminal.job_id().as_str()]).await.expect("park terminal owner");
    // These older frozen owners crossed the uncertainty boundary before a
    // crash, so no provider acceptance or authoritative terminal proof exists.
    for _ in 0..2 {
        store.execute(
            "INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, thread_id, class, message_count, context_xml, status, attempt_count, policy_error_count, approved_payload_xml, approved_publish_options_xml, queue_acceptance_may_exist, next_attempt_at_ms, created_at_ms, updated_at_ms) SELECT ?, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, thread_id, class, message_count, context_xml, 'failed', 5, 0, approved_payload_xml, approved_publish_options_xml, 1, 1, created_at_ms, updated_at_ms FROM notification_outbox WHERE job_id = ?",
            waddle_server::db_params![uuid::Uuid::new_v4().to_string(), terminal.job_id().as_str()],
        ).await.expect("unknown parked owner");
    }
    push.remove_registered_nodes_for_owner(
        &recipient,
        "push.example.com",
        Some(target.node().as_str()),
    )
    .await
    .expect("terminal proof");
    for _ in 0..2 {
        assert!(
            store
                .drain_due_outbox_jobs(
                    &push,
                    &registrations,
                    &inbox,
                    &blocking,
                    &bare("push.example.com"),
                    1
                )
                .await
                .expect("bounded unresolved probe")
                .is_empty(),
            "one unresolved owner per pass keeps later proof outside this batch"
        );
    }
    // Reconstruct the store to prove fairness survives worker/store restart.
    let resumed = NotificationOutboxStore::try_new(store.db.clone())
        .await
        .expect("restart store");
    let completed = resumed
        .drain_due_outbox_jobs(
            &push,
            &registrations,
            &inbox,
            &blocking,
            &bare("push.example.com"),
            1,
        )
        .await
        .expect("later terminal probe");
    assert!(
        matches!(completed.as_slice(), [NotificationOutboxPublishOutcome::Failed { job_id }] if job_id == terminal.job_id())
    );
    let mut rows = store.query("SELECT COUNT(*) FROM notification_outbox WHERE status = 'failed' AND queue_acceptance_may_exist = 1", ()).await.expect("remaining custody");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("count"),
        2
    );
}
