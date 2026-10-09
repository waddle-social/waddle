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
    use crate::push_service::{
        DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
    };
    let outbox = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let registrations =
        crate::push_registrations::DatabasePushRegistrationStore::new(fixture.db.clone())
            .await
            .expect("registrations");
    let push = DatabasePushServiceStore::new_with_secret_key(
        fixture.db.clone(),
        b"notification-ancestry-secret",
    )
    .await
    .expect("push service");
    let candidate = candidate("provider-bound");
    let owner = candidate.recipient_bare_jid.clone();
    let service: BareJid = "push.example.com".parse().expect("service");
    let victim = push
        .ensure_node(&owner, "victim-app")
        .await
        .expect("victim node");
    let other = push
        .ensure_node(&owner, "other-app")
        .await
        .expect("other node");
    for (node, device) in [
        (victim.node(), "victim-device"),
        (other.node(), "other-device"),
    ] {
        push.upsert_device(
            &owner,
            PushDeviceRegistration::new(device, node, PushDevicePlatform::Fcm, "test"),
        )
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
    push.upsert_device(
        &owner,
        PushDeviceRegistration::new(
            "other-device",
            other.node(),
            PushDevicePlatform::Fcm,
            "test",
        ),
    )
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
    push.remove_registered_nodes_for_owner(&owner, service.as_str(), Some(victim.node()))
        .await
        .expect("revoke victim registration");
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
        b"notification-version-secret",
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
