//! XEP-0191 blocking gates across the candidate and publish workers.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use crate::support::*;
use jid::Jid;
use waddle_server::db::Database;
use waddle_server::notification_outbox::*;
use waddle_xmpp::push::PushSubscriptionStore;
use waddle_xmpp::xep::xep0191::BlockingStorage;

async fn drain_dm_outbox_with_blocking(
    archive_id: &str,
    blocking: &dyn BlockingStorage,
) -> (
    Vec<NotificationOutboxPublishOutcome>,
    usize,
    Vec<NotificationOutboxJob>,
) {
    drain_dm_outbox_with_sender_jid(
        archive_id,
        "bob@example.com/phone".parse().expect("sender JID"),
        blocking,
    )
    .await
}

async fn drain_dm_outbox_with_sender_jid(
    archive_id: &str,
    sender_jid: Jid,
    blocking: &dyn BlockingStorage,
) -> (
    Vec<NotificationOutboxPublishOutcome>,
    usize,
    Vec<NotificationOutboxJob>,
) {
    let store = store().await;
    let recipient = bare("alice@example.com");
    let sender = sender_jid.to_bare();
    let push_db_name = format!("push-service-{archive_id}");
    let push_service = waddle_server::push_service::DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory(&push_db_name).await.unwrap(),
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
    let target = NotificationOutboxTarget::new(
        bare("push.example.com"),
        PushServiceNodeName::new(push_node.node()).expect("push node target"),
    );
    enqueue_jobs_for_test(
        &store,
        &candidate_for_sender_jid(&recipient, sender_jid, archive_id),
        &[target],
    )
    .await;
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
    let inbox = inbox_with_unread(&recipient, &sender, 1).await;

    let outcomes = store
        .drain_due_outbox_jobs(
            &push_service,
            &push_store,
            &inbox,
            blocking,
            &bare("push.example.com"),
            16,
        )
        .await
        .expect("drain outbox");
    let queued_push_job_count = push_service
        .queued_publish_jobs()
        .await
        .expect("queued push jobs")
        .len();
    let pending = store.pending_outbox_jobs().await.expect("pending jobs");
    (outcomes, queued_push_job_count, pending)
}

#[tokio::test]
async fn candidate_worker_filters_full_jid_block_before_bare_conversation_coalescing() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    register_push_target(&push_store, &recipient, &target).await;
    blocking.set_blocklist_jids(
        recipient.clone(),
        vec!["bob@example.com/phone".parse().expect("blocked sender")],
    );

    let blocked_phone = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-blocked-phone",
    );
    let allowed_laptop = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/laptop".parse().expect("laptop sender"),
        "archive-allowed-laptop",
    );
    store
        .insert_candidate(&blocked_phone)
        .await
        .expect("blocked insert");
    store
        .insert_candidate(&allowed_laptop)
        .await
        .expect("allowed insert");

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        2
    );

    let jobs = store.pending_outbox_jobs().await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].conversation_jid(), &bare("bob@example.com"));
    assert_eq!(jobs[0].sender_jid().to_string(), "bob@example.com/laptop");
    assert_eq!(jobs[0].message_count(), 1);
}

#[tokio::test]
async fn candidate_worker_applies_xep0191_to_groupchat_notifications() {
    let store = store().await;
    let target = target();
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let recipient = bare("alice@example.com");
    let room = bare("team@muc.example.com");
    register_push_target(&push_store, &recipient, &target).await;
    blocking.set_blocklist_jids(recipient.clone(), vec![Jid::from(room.clone())]);

    let candidate = groupchat_candidate_for(
        &recipient,
        &room,
        "team@muc.example.com/bob".parse().expect("room occupant"),
        "archive-blocked-groupchat",
        NotificationClass::ChannelMention,
    );
    store
        .insert_candidate(&candidate)
        .await
        .expect("groupchat insert");

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        1
    );

    assert!(
        store.pending_outbox_jobs().await.expect("jobs").is_empty(),
        "XEP-0191-blocked groupchat notifications must not enqueue outbox jobs"
    );
}

#[tokio::test]
async fn xep0191_full_jid_block_added_after_coalescing_suppresses_dm_push_job() {
    let store = store().await;
    let recipient = bare("alice@example.com");
    let push_store = waddle_xmpp::push::InMemoryPushStore::new();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let projection = settings_projection().await;
    let room_policy = StubRoomPolicy::new();
    let push_service = waddle_server::push_service::DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("push-service-coalesced-full-jid-block")
            .await
            .unwrap(),
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
    let target = NotificationOutboxTarget::new(
        bare("push.example.com"),
        PushServiceNodeName::new(push_node.node()).expect("push node target"),
    );
    register_push_target(&push_store, &recipient, &target).await;

    let phone = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/phone".parse().expect("phone sender"),
        "archive-coalesced-phone",
    );
    let laptop = candidate_for_sender_jid(
        &recipient,
        "bob@example.com/laptop".parse().expect("laptop sender"),
        "archive-coalesced-laptop",
    );
    store.insert_candidate(&phone).await.expect("phone insert");
    store
        .insert_candidate(&laptop)
        .await
        .expect("laptop insert");

    assert_eq!(
        store
            .drain_pending_candidates_into_outbox(
                &push_store,
                &blocking,
                &projection,
                drain_deps_with_noop_activity(&room_policy, &NoopDndReader, noop_activity_reader()),
                &bare("push.example.com"),
                16,
            )
            .await
            .expect("drain candidates"),
        2
    );
    let pending = store.pending_outbox_jobs().await.expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message_count(), 2);
    assert!(pending[0]
        .sender_jids()
        .contains(&"bob@example.com/phone".parse().expect("phone sender")));

    blocking.set_blocklist_jids(
        recipient.clone(),
        vec!["bob@example.com/phone".parse().expect("blocked sender")],
    );

    let publish_push_store = waddle_xmpp::push::InMemoryPushStore::new();
    register_push_target(&publish_push_store, &recipient, &target).await;
    let inbox = inbox_with_unread(&recipient, &bare("bob@example.com"), 2).await;

    let outcomes = store
        .drain_due_outbox_jobs(
            &push_service,
            &publish_push_store,
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
        "a coalesced job that includes a blocked full sender JID must not publish"
    );
    assert!(store
        .pending_outbox_jobs()
        .await
        .expect("pending jobs")
        .is_empty());
}

#[tokio::test]
async fn xep0191_blocked_dm_outbox_job_does_not_publish_push_notification() {
    let recipient = bare("alice@example.com");
    let sender = bare("bob@example.com");
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    blocking.set_blocklist(recipient.clone(), vec![sender.clone()]);
    let (outcomes, queued_push_job_count, pending_jobs) =
        drain_dm_outbox_with_blocking("archive-blocked-bare", &blocking).await;

    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes[0],
        NotificationOutboxPublishOutcome::Failed { .. }
    ));
    assert_eq!(
        queued_push_job_count, 0,
        "XEP-0191-blocked DMs must not enqueue XEP-0357 push publish jobs"
    );
    assert!(
        pending_jobs.is_empty(),
        "blocked notification jobs should become terminal instead of retrying forever"
    );
}

#[tokio::test]
async fn xep0191_full_jid_block_suppresses_dm_push_candidate() {
    let recipient = bare("alice@example.com");
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    blocking.set_blocklist_jids(
        recipient,
        vec!["bob@example.com/phone".parse().expect("full blocked JID")],
    );

    let (outcomes, queued_push_job_count, pending_jobs) =
        drain_dm_outbox_with_blocking("archive-blocked-full", &blocking).await;

    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes[0],
        NotificationOutboxPublishOutcome::Failed { .. }
    ));
    assert_eq!(queued_push_job_count, 0);
    assert!(pending_jobs.is_empty());
}

#[tokio::test]
async fn xep0191_full_jid_block_does_not_suppress_other_sender_resource() {
    let recipient = bare("alice@example.com");
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    blocking.set_blocklist_jids(
        recipient,
        vec!["bob@example.com/phone".parse().expect("full blocked JID")],
    );

    let (outcomes, queued_push_job_count, pending_jobs) = drain_dm_outbox_with_sender_jid(
        "archive-full-block-other-resource",
        "bob@example.com/laptop".parse().expect("sender resource"),
        &blocking,
    )
    .await;

    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes[0],
        NotificationOutboxPublishOutcome::Published { .. }
    ));
    assert_eq!(
        queued_push_job_count, 1,
        "a full-JID XEP-0191 block must not suppress another resource from the same bare JID"
    );
    assert!(pending_jobs.is_empty());
}

#[tokio::test]
async fn xep0191_domain_block_suppresses_dm_push_candidate() {
    let recipient = bare("alice@example.com");
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    blocking.set_blocklist_jids(
        recipient,
        vec!["example.com".parse().expect("domain blocked JID")],
    );

    let (outcomes, queued_push_job_count, pending_jobs) =
        drain_dm_outbox_with_blocking("archive-blocked-domain", &blocking).await;

    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes[0],
        NotificationOutboxPublishOutcome::Failed { .. }
    ));
    assert_eq!(queued_push_job_count, 0);
    assert!(pending_jobs.is_empty());
}

#[tokio::test]
async fn xep0191_blocklist_load_error_preserves_outbox_job_without_spending_attempt() {
    let (outcomes, queued_push_job_count, pending_jobs) =
        drain_dm_outbox_with_blocking("archive-blocking-storage-error", &FailingBlockingStorage)
            .await;

    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes[0],
        NotificationOutboxPublishOutcome::RetryScheduled { .. }
    ));
    assert_eq!(
        queued_push_job_count, 0,
        "policy-read failures must not publish before XEP-0191 can be enforced"
    );
    assert_eq!(pending_jobs.len(), 1);
    assert_eq!(pending_jobs[0].status(), NotificationOutboxStatus::Queued);
    assert_eq!(pending_jobs[0].attempt_count(), 0);
    assert_eq!(pending_jobs[0].policy_error_count(), 1);
    assert!(pending_jobs[0].claim_token().is_none());
}
