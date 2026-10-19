use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{CanonicalMessageRepository, EffectIntentRepository};
use crate::notification_outbox::test_support::{candidate, target_named};
use waddle_xmpp::ingress::{
    IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    SemanticDigest,
};

async fn attach(fixture: &IngressFixture, candidate: &NotificationCandidate) -> MessageKey {
    let key = MessageKey::new();
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: candidate.recipient_bare_jid.clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: candidate.conversation_jid.clone(),
            archive_stanza_id: candidate.archive_stanza_id.clone(),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let mut tx = fixture.uow.begin().await.expect("canonical tx");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [1; 32]).expect("digest"),
        None,
    )
    .await
    .expect("canonical parent");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("canonical intent");
    tx.commit().await.expect("commit canonical intent");
    let mut tx = fixture.db.begin_immediate().await.expect("candidate tx");
    NotificationOutboxStore::insert_candidate_in_transaction(
        &mut tx,
        candidate,
        crate::time::now_ms(),
    )
    .await
    .expect("candidate");
    NotificationOutboxStore::attach_candidate_lineage_in_transaction(
        &mut tx,
        key,
        &intent.semantic_key(),
        candidate,
    )
    .await
    .expect("canonical scheduling custody");
    tx.commit().await.expect("commit");
    key
}

async fn fanout(
    store: &NotificationOutboxStore,
    candidate: &NotificationCandidate,
    targets: &[NotificationOutboxTarget],
) {
    let mut tx = store.db.begin_immediate().await.expect("drain tx");
    lock_candidate_ancestry_tx(&mut tx, candidate)
        .await
        .expect("parent lock");
    let now = crate::time::now_ms();
    mark_candidate_outboxed_tx(&mut tx, candidate, now)
        .await
        .expect("outboxed");
    for target in targets {
        enqueue_outbox_job_tx(
            &mut tx,
            candidate,
            target,
            &build_waddle_context(candidate),
            &RichSummary::minimal(),
            now,
        )
        .await
        .expect("job");
    }
    settle_candidate_ancestry_tx(&mut tx, candidate)
        .await
        .expect("handoff");
    tx.commit().await.expect("commit drain");
}

async fn notification_ancestry_fanout_coalescing_and_late_parent(fixture: IngressFixture) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let first = candidate("archive-first");
    let second = candidate("archive-second");
    let targets = [target_named("node-one"), target_named("node-two")];
    attach(&fixture, &first).await;
    fanout(&store, &first, &targets).await;
    attach(&fixture, &second).await;
    fanout(&store, &second, &targets).await;
    let jobs = store.pending_outbox_jobs().await.expect("coalesced jobs");
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().all(|job| job.message_count() == 2));
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        4,
        "both parents survive each fanout target"
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        4
    );
    let late_key = attach(&fixture, &first).await;
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        6,
        "a late canonical parent follows both existing jobs"
    );
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("provider terminal tx");
    lock_outbox_ancestry_tx(&mut tx, jobs[0].job_id.as_str())
        .await
        .expect("parent lock");
    settle_outbox_ancestry_tx(&mut tx, jobs[0].job_id.as_str())
        .await
        .expect("provider terminal");
    tx.commit().await.expect("commit terminal");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        3,
        "one outstanding target retains every parent"
    );
    let mut tx = fixture.uow.begin().await.expect("replay tx");
    let intent = EffectIntentRepository::load(&mut tx, late_key)
        .await
        .expect("recorded")
        .remove(0);
    tx.commit().await.expect("release intent read");
    let mut tx = fixture
        .db
        .begin_immediate()
        .await
        .expect("replay candidate tx");
    NotificationOutboxStore::attach_candidate_lineage_in_transaction(
        &mut tx,
        late_key,
        &intent.semantic_key(),
        &first,
    )
    .await
    .expect("same-canonical replay");
    tx.commit().await.expect("replay commit");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        3,
        "settled same-key references do not reopen"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_notification_ancestry_fanout_coalescing_and_late_parent() {
    notification_ancestry_fanout_coalescing_and_late_parent(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_notification_ancestry_fanout_coalescing_and_late_parent() {
    if let Some(fixture) = IngressFixture::postgres("notification_ancestry").await {
        notification_ancestry_fanout_coalescing_and_late_parent(fixture).await;
    }
}

async fn notification_provider_binding_and_revocation(fixture: IngressFixture) {
    use crate::push_service::test_support::{
        apns_registration, fcm_registration, with_fake_apns, with_fake_fcm, FakeFcmSender,
        APNS_TEST_TOPIC, FCM_TEST_APP_ID,
    };
    use crate::push_service::DatabasePushServiceStore;
    let outbox = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let registrations =
        crate::push_registrations::DatabasePushRegistrationStore::new(fixture.db.clone())
            .await
            .expect("registrations");
    // Two nodes of one owner that both deliver: the victim is the
    // Android app (fake FCM), the other the Apple app (fake APNs).
    let push = with_fake_apns(with_fake_fcm(
        DatabasePushServiceStore::new_with_secret_key(
            fixture.db.clone(),
            &rand::random::<[u8; 32]>(),
        )
        .await
        .expect("push service"),
        FakeFcmSender::default(),
    ));
    let candidate = candidate("provider-bound");
    let owner = candidate.recipient_bare_jid.clone();
    let service: BareJid = "push.example.com".parse().expect("service");
    let victim = push
        .ensure_node(&owner, FCM_TEST_APP_ID)
        .await
        .expect("victim node");
    let other = push
        .ensure_node(&owner, APNS_TEST_TOPIC)
        .await
        .expect("other node");
    for (node, registration) in [
        (
            victim.node(),
            fcm_registration("victim-device", victim.node()),
        ),
        (
            other.node(),
            apns_registration("other-device", other.node()),
        ),
    ] {
        push.upsert_device(&owner, registration)
            .await
            .expect("registered device");
        push.register_first_party_node_for_owner(&owner, service.as_str(), node, None)
            .await
            .expect("registration");
    }
    attach(&fixture, &candidate).await;
    fanout(
        &outbox,
        &candidate,
        &[NotificationOutboxTarget::new(
            service.clone(),
            PushServiceNodeName::new(victim.node()).expect("node"),
        )],
    )
    .await;
    let claimed = outbox
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    let inbox = crate::notification_outbox::test_support::inbox_with_unread(
        &owner,
        &candidate.conversation_jid,
        1,
    )
    .await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    outbox
        .publish_claimed_job(&claimed, &push, &registrations, &inbox, &blocking, &service)
        .await
        .expect("approved durable acceptance");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("push_publish_jobs WHERE ancestry_job_id IS NOT NULL")
            .await,
        1
    );
    // Knowing a canonical PubSub UUID does not grant the right to settle it
    // through another owned node's provider scheduler.
    push.upsert_device(&owner, apns_registration("other-device", other.node()))
        .await
        .expect("other device");
    push.publish_registered_notification_from_user_server_with_publish_options(
        service.as_str(),
        other.node(),
        &claimed.to_xep0357_pubsub_item(),
        &owner,
        None,
    )
    .await
    .expect("independent node publish");
    assert_eq!(
        fixture
            .count("push_publish_jobs WHERE ancestry_job_id IS NULL AND status = 'published'")
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1,
        "a forged other-node terminal result must not release the victim parent"
    );
    // Even a byte-identical wire publication at the same authorized tuple
    // creates independent work and cannot acquire the host's capability.
    push.publish_registered_notification_from_user_server_with_publish_options(
        service.as_str(),
        victim.node(),
        &claimed.to_xep0357_pubsub_item(),
        &owner,
        None,
    )
    .await
    .expect("identical wire publication");
    assert_eq!(fixture.count("push_publish_jobs WHERE ancestry_job_id IS NULL AND acceptance_scope = 'wire' AND status = 'published'").await, 2);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1,
        "a wire publication cannot settle canonical custody even at the same tuple"
    );
    fixture.execute("UPDATE notification_outbox SET status = 'failed', queue_acceptance_may_exist = 1, published_at_ms = NULL WHERE job_id = ?", crate::db_params![claimed.job_id.as_str()]).await;
    fixture
        .execute(
            "UPDATE push_publish_jobs SET upstream_completed = 0 WHERE ancestry_job_id = ?",
            crate::db_params![claimed.job_id.as_str()],
        )
        .await;
    push.remove_registered_nodes_for_owner(&owner, service.as_str(), Some(victim.node()))
        .await
        .expect("revoke victim registration");
    assert_eq!(
        fixture
            .count("push_publish_jobs WHERE ancestry_job_id IS NOT NULL AND upstream_completed = 1")
            .await,
        1,
        "same-database explicit retirement acknowledges its exact canonical provider owner"
    );
    assert_eq!(fixture.count("notification_outbox WHERE status = 'failed' AND queue_acceptance_may_exist = 0 AND last_error = 'registration-revoked'").await, 1);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        0,
        "explicit registration revocation terminates scheduling custody"
    );
    assert_eq!(
        fixture
            .count("notification_outbox_lineage WHERE settled_at_ms IS NULL")
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("push_publish_jobs WHERE ancestry_job_id IS NOT NULL AND status = 'failed'")
            .await,
        1,
        "revocation records refusal rather than provider success or disappearing evidence"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_notification_provider_binding_and_revocation() {
    notification_provider_binding_and_revocation(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_notification_provider_binding_and_revocation() {
    if let Some(fixture) = IngressFixture::postgres("notification_binding").await {
        notification_provider_binding_and_revocation(fixture).await;
    }
}

async fn notification_lost_backing_marker_preserves_newer_wire(fixture: IngressFixture) {
    use crate::push_service::{
        DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
    };
    use waddle_xmpp::pubsub::{PubSubItem, PubSubStorage};
    let outbox = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let registrations =
        crate::push_registrations::DatabasePushRegistrationStore::new(fixture.db.clone())
            .await
            .expect("registrations");
    // Backing uses a genuinely different database from queue/Foundation.
    let backing = std::sync::Arc::new(
        crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("independent backing"),
    );
    let service: BareJid = "push.example.com".parse().expect("service");
    let push = DatabasePushServiceStore::new_with_secret_key_and_pubsub(
        fixture.db.clone(),
        &rand::random::<[u8; 32]>(),
        service.clone(),
        backing.clone(),
    )
    .await
    .expect("push service");
    let candidate = candidate("backing-original");
    let owner = candidate.recipient_bare_jid.clone();
    let node = push
        .ensure_node(&owner, "versioned-app")
        .await
        .expect("node");
    push.upsert_device(
        &owner,
        PushDeviceRegistration::new("device", node.node(), PushDevicePlatform::Fcm, "test"),
    )
    .await
    .expect("device");
    push.register_first_party_node_for_owner(&owner, service.as_str(), node.node(), None)
        .await
        .expect("register");
    let mut config = waddle_xmpp::pubsub::NodeConfig::push_service();
    config.max_items = 1;
    backing
        .update_node_config(&service, node.node(), &config)
        .await
        .expect("max-items1 fixture");
    attach(&fixture, &candidate).await;
    fanout(
        &outbox,
        &candidate,
        &[NotificationOutboxTarget::new(
            service.clone(),
            PushServiceNodeName::new(node.node()).expect("node"),
        )],
    )
    .await;
    let claimed = outbox
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    let inbox = crate::notification_outbox::test_support::inbox_with_unread(
        &owner,
        &candidate.conversation_jid,
        1,
    )
    .await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    outbox
        .publish_claimed_job(&claimed, &push, &registrations, &inbox, &blocking, &service)
        .await
        .expect("first canonical acceptance");
    let replacement = PubSubItem::new(
        Some("newer-wire-item".to_string()),
        Some(Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build()),
    );
    push.publish_registered_notification_from_user_server_with_publish_options(
        service.as_str(),
        node.node(),
        &replacement,
        &owner,
        None,
    )
    .await
    .expect("newer wire publication");
    fixture.execute("UPDATE push_publish_jobs SET backing_state = 'pending', backing_published_at_ms = NULL WHERE ancestry_job_id = ?", crate::db_params![claimed.job_id.as_str()]).await;
    fixture.execute("UPDATE notification_outbox SET status = 'queued', published_at_ms = NULL WHERE job_id = ?", crate::db_params![claimed.job_id.as_str()]).await;
    let retry = outbox
        .claim_due_outbox_jobs(1)
        .await
        .expect("reclaim")
        .remove(0);
    outbox
        .publish_claimed_job(&retry, &push, &registrations, &inbox, &blocking, &service)
        .await
        .expect("lost completion marker retry");
    let latest = backing
        .get_items(&service, node.node(), None, &[])
        .await
        .expect("backing items");
    assert_eq!(latest.len(), 1);
    assert_eq!(
        latest[0].id, "newer-wire-item",
        "a delayed canonical projection cannot evict the newer max-items1 item"
    );
    assert_eq!(fixture.count("push_publish_jobs WHERE acceptance_scope = 'canonical' AND backing_state = 'superseded' AND status = 'queued'").await, 1, "projection supersession does not fabricate provider acceptance");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_notification_lost_backing_marker_preserves_newer_wire() {
    notification_lost_backing_marker_preserves_newer_wire(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_notification_lost_backing_marker_preserves_newer_wire() {
    if let Some(fixture) = IngressFixture::postgres("notification_version").await {
        notification_lost_backing_marker_preserves_newer_wire(fixture).await;
    }
}

#[tokio::test]
async fn postgres_parked_parent_contention_does_not_block_fresh_outbox_work() {
    use crate::ingress_uow::EffectDescendantRepository;
    use crate::notification_outbox::test_support::inbox_with_unread;
    use crate::push_service::test_support::{
        fcm_registration, with_fake_fcm, FakeFcmSender, FCM_TEST_APP_ID,
    };
    use crate::push_service::DatabasePushServiceStore;
    use waddle_xmpp::push::{InMemoryPushStore, PushSubscription};
    let Some(fixture) = IngressFixture::postgres("parked_outbox_fairness").await else {
        return;
    };
    let outbox = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let provider_db = Database::in_memory("parked-fairness-provider")
        .await
        .expect("provider db");
    crate::push_registrations::DatabasePushRegistrationStore::new(provider_db.clone())
        .await
        .expect("registration schema");
    let push = with_fake_fcm(
        DatabasePushServiceStore::new_with_secret_key(provider_db, &rand::random::<[u8; 32]>())
            .await
            .expect("push"),
        FakeFcmSender::default(),
    );
    let parked_candidate = candidate("parked-parent-contention");
    let owner = parked_candidate.recipient_bare_jid.clone();
    let service: BareJid = "push.example.com".parse().expect("service");
    let node = push
        .ensure_node(&owner, FCM_TEST_APP_ID)
        .await
        .expect("node");
    push.upsert_device(&owner, fcm_registration("fairness-device", node.node()))
        .await
        .expect("device");
    push.register_first_party_node_for_owner(&owner, service.as_str(), node.node(), None)
        .await
        .expect("registration");
    let registrations = InMemoryPushStore::new();
    registrations
        .register(PushSubscription {
            user_jid: owner.to_string(),
            service_jid: service.to_string(),
            node: Some(node.node().to_string()),
            publish_options: None,
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
        .expect("user registration");
    let target = NotificationOutboxTarget::new(
        service.clone(),
        PushServiceNodeName::new(node.node()).expect("node"),
    );
    let parent = attach(&fixture, &parked_candidate).await;
    fanout(&outbox, &parked_candidate, std::slice::from_ref(&target)).await;
    let parked = outbox
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    let inbox = inbox_with_unread(&owner, &parked_candidate.conversation_jid, 1).await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    outbox
        .publish_claimed_job(&parked, &push, &registrations, &inbox, &blocking, &service)
        .await
        .expect("canonical acceptance");
    // The separate provider store can finish while the upstream owner retains
    // custody after losing its acceptance reply and exhausting its budget.
    push.drain_queued_notification_publish_jobs(1)
        .await
        .expect("provider completion");
    fixture.execute("UPDATE notification_outbox SET status = 'failed', queue_acceptance_may_exist = 1, attempt_count = 5, published_at_ms = NULL WHERE job_id = ?", crate::db_params![parked.job_id.as_str()]).await;
    let fresh = candidate("unrelated-fresh-owner");
    attach(&fixture, &fresh).await;
    fanout(&outbox, &fresh, &[target]).await;
    let mut held_parent = fixture.db.begin().await.expect("hold parent");
    EffectDescendantRepository::lock_raw(&mut held_parent, parent)
        .await
        .expect("parent lock");
    let outcomes = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        outbox.drain_due_outbox_jobs(&push, &registrations, &inbox, &blocking, &service, 1),
    )
    .await
    .expect("parked parent must not stall drain")
    .expect("isolated parent contention");
    assert!(
        matches!(outcomes.as_slice(), [NotificationOutboxPublishOutcome::Published { job_id, .. }] if job_id != parked.job_id())
    );
    assert_eq!(
        fixture
            .count("notification_outbox WHERE status = 'failed' AND queue_acceptance_may_exist = 1")
            .await,
        1,
        "contended parked custody stays intact"
    );
    held_parent.commit().await.expect("release parent");
    let reconciled = outbox
        .drain_due_outbox_jobs(&push, &registrations, &inbox, &blocking, &service, 1)
        .await
        .expect("reconcile after release");
    assert!(
        matches!(reconciled.as_slice(), [NotificationOutboxPublishOutcome::Published { job_id, .. }] if job_id == parked.job_id())
    );
    fixture.close().await;
}

async fn explicit_revocation_retires_parked_boundary_without_provider_row(fixture: IngressFixture) {
    use crate::push_service::test_support::{fcm_registration, FCM_TEST_APP_ID};
    use crate::push_service::DatabasePushServiceStore;
    let outbox = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    crate::push_registrations::DatabasePushRegistrationStore::new(fixture.db.clone())
        .await
        .expect("registration schema");
    let push = DatabasePushServiceStore::new_with_secret_key(
        fixture.db.clone(),
        &rand::random::<[u8; 32]>(),
    )
    .await
    .expect("push");
    let notification = candidate("parked-before-provider-call");
    let owner = notification.recipient_bare_jid.clone();
    let service: BareJid = "push.example.com".parse().expect("service");
    let node = push
        .ensure_node(&owner, FCM_TEST_APP_ID)
        .await
        .expect("node");
    push.upsert_device(&owner, fcm_registration("boundary-device", node.node()))
        .await
        .expect("device");
    push.register_first_party_node_for_owner(&owner, service.as_str(), node.node(), None)
        .await
        .expect("registration");
    attach(&fixture, &notification).await;
    fanout(
        &outbox,
        &notification,
        &[NotificationOutboxTarget::new(
            service.clone(),
            PushServiceNodeName::new(node.node()).expect("node"),
        )],
    )
    .await;
    let job = outbox
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    let payload = job
        .to_xep0357_pubsub_item()
        .payload
        .expect("frozen payload");
    // Crash after freezing and persisting acceptance uncertainty, before the
    // first provider call. Exhaustion parks custody without any queue proof.
    fixture.execute("UPDATE notification_outbox SET status = 'failed', queue_acceptance_may_exist = 1, approved_payload_xml = ?, attempt_count = 5, claimed_at_ms = NULL, claim_token = NULL WHERE job_id = ?", crate::db_params![String::from(&payload), job.job_id.as_str()]).await;
    assert_eq!(fixture.count("push_publish_jobs").await, 0);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        1
    );
    assert_eq!(
        push.remove_registered_nodes_for_owner(&owner, "other-push.example.com", None)
            .await
            .expect("other service"),
        0
    );
    assert_eq!(
        fixture
            .count("notification_outbox WHERE queue_acceptance_may_exist = 1")
            .await,
        1,
        "a different service has no retirement authority"
    );
    assert_eq!(
        outbox
            .prune_completed_before(i64::MAX, 16)
            .await
            .expect("parked prune")
            .jobs_deleted,
        0
    );
    // Lost registration metadata does not invalidate the explicit owner's
    // service-wide retirement request; no live node list is required.
    fixture.execute("DELETE FROM push_registrations", ()).await;
    assert_eq!(
        push.remove_registered_nodes_for_owner(&owner, service.as_str(), None)
            .await
            .expect("explicit revocation with empty registration list"),
        0
    );
    assert_eq!(fixture.count("notification_outbox WHERE status = 'failed' AND queue_acceptance_may_exist = 0 AND last_error = 'registration-revoked' AND claim_token IS NULL").await, 1);
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
    let mut rows = outbox
        .query(
            "SELECT approved_payload_xml FROM notification_outbox WHERE job_id = ?",
            crate::db_params![job.job_id.as_str()],
        )
        .await
        .expect("retained evidence");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("outbox")
            .get::<String>(0)
            .expect("payload"),
        String::from(&payload)
    );
    assert_eq!(
        outbox
            .prune_completed_before(i64::MAX, 16)
            .await
            .expect("retention tail")
            .jobs_deleted,
        0
    );
    fixture
        .execute(
            "UPDATE notification_outbox SET updated_at_ms = 42 WHERE job_id = ?",
            crate::db_params![job.job_id.as_str()],
        )
        .await;
    push.remove_registered_nodes_for_owner(&owner, service.as_str(), None)
        .await
        .expect("idempotent explicit revocation");
    assert_eq!(fixture.count("notification_outbox WHERE updated_at_ms = 42 AND last_error = 'registration-revoked'").await, 1, "repeated retirement must not restart the evidence retention tail");
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_explicit_revocation_retires_parked_boundary_without_provider_row() {
    explicit_revocation_retires_parked_boundary_without_provider_row(
        IngressFixture::sqlite().await,
    )
    .await;
}

#[tokio::test]
async fn postgres_explicit_revocation_retires_parked_boundary_without_provider_row() {
    if let Some(fixture) = IngressFixture::postgres("parked_boundary_revocation").await {
        explicit_revocation_retires_parked_boundary_without_provider_row(fixture).await;
    }
}
