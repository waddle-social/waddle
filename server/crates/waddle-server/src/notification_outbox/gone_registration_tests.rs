//! XEP-0357 publish errors retire a gone target without consuming retry budget.

use super::{
    BareJid, NotificationOutboxJob, NotificationOutboxPublishOutcome, NotificationOutboxStore,
    PushSubscriptionStore, STATUS_FAILED,
};
use crate::ingress::test_support::IngressFixture;
use crate::notification_outbox::test_support::{
    candidate, enqueue_jobs_for_test, inbox_with_unread, target_named,
};
use crate::notification_outbox::NotificationCandidate;
use crate::push_registrations::DatabasePushRegistrationStore;
use crate::push_service::test_support::{fcm_registration, FCM_TEST_APP_ID};
use crate::push_service::DatabasePushServiceStore;

async fn configured_push(fixture: &IngressFixture, service: &BareJid) -> DatabasePushServiceStore {
    DatabasePushServiceStore::new_with_secret_key_and_pubsub(
        fixture.db.clone(),
        &rand::random::<[u8; 32]>(),
        service.clone(),
        std::sync::Arc::new(waddle_xmpp::pubsub::InMemoryPubSubStorage::new()),
    )
    .await
    .expect("configured push")
}

async fn attach_outbox_custody(
    fixture: &IngressFixture,
    notification: &NotificationCandidate,
    job: &NotificationOutboxJob,
) {
    use crate::ingress_uow::{
        CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
    };
    use waddle_xmpp::ingress::{
        IngressEffectIntent, MessageKey, NotificationActivityMutation,
        NotificationCandidateOutcome, SemanticDigest,
    };
    let key = MessageKey::new();
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: notification.recipient_bare_jid.clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: notification.conversation_jid.clone(),
            archive_stanza_id: notification.archive_stanza_id.clone(),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    let mut tx = fixture.uow.begin().await.expect("canonical transaction");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [37; 32]).expect("digest"),
        None,
    )
    .await
    .expect("canonical parent");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("canonical intent");
    EffectDescendantRepository::attach(
        &mut tx,
        key,
        &intent.semantic_key(),
        uuid::Uuid::parse_str(job.job_id.as_str()).expect("delivery identity"),
    )
    .await
    .expect("canonical custody");
    tx.commit().await.expect("canonical commit");
}

async fn deleted_node_disables_only_its_registration(fixture: IngressFixture) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let outbox = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("outbox");
    let registrations = DatabasePushRegistrationStore::try_new(fixture.db.clone())
        .await
        .expect("registrations");
    let notification = candidate("deleted-push-node");
    let owner = notification.recipient_bare_jid();
    let service: BareJid = "push.example.com".parse().expect("service");
    let push = configured_push(&fixture, &service).await;
    let deleted = push
        .ensure_node(owner, FCM_TEST_APP_ID)
        .await
        .expect("deleted node");
    let healthy = push
        .ensure_node(owner, "healthy-app")
        .await
        .expect("healthy node");
    for node in [&deleted, &healthy] {
        push.upsert_device(owner, fcm_registration(node.node(), node.node()))
            .await
            .expect("device");
        push.register_first_party_node_for_owner(owner, service.as_str(), node.node(), None)
            .await
            .expect("enabled registration");
    }
    let mut foreign_service_registration = registrations
        .get_for_user(&owner.to_string())
        .await
        .expect("initial registrations")
        .into_iter()
        .find(|registration| registration.node.as_deref() == Some(deleted.node()))
        .expect("registration to copy");
    foreign_service_registration.service_jid = "other-push.example.com".to_owned();
    registrations
        .register(foreign_service_registration.clone())
        .await
        .expect("unrelated service registration");
    foreign_service_registration.user_jid = "other-owner@example.com".to_owned();
    foreign_service_registration.service_jid = service.to_string();
    registrations
        .register(foreign_service_registration)
        .await
        .expect("unrelated owner registration");
    fixture
        .execute(
            "DELETE FROM push_nodes WHERE node = ?",
            crate::db_params![deleted.node()],
        )
        .await;
    enqueue_jobs_for_test(
        &outbox,
        &notification,
        &[target_named(deleted.node()), target_named(healthy.node())],
    )
    .await;
    let claims = outbox.claim_due_outbox_jobs(2).await.expect("claims");
    let gone = claims
        .iter()
        .find(|job| job.node.as_str() == deleted.node())
        .expect("gone claim");
    let live = claims
        .iter()
        .find(|job| job.node.as_str() == healthy.node())
        .expect("healthy claim");
    let inbox = inbox_with_unread(owner, notification.conversation_jid(), 1).await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    assert!(matches!(
        outbox
            .publish_claimed_job(gone, &push, &registrations, &inbox, &blocking, &service)
            .await
            .expect("gone outcome"),
        NotificationOutboxPublishOutcome::Failed { .. }
    ));
    let enabled = registrations
        .get_for_user(&owner.to_string())
        .await
        .expect("enabled targets");
    assert_eq!(enabled.len(), 2);
    assert!(enabled
        .iter()
        .any(|registration| registration.service_jid == service.as_str()
            && registration.node.as_deref() == Some(healthy.node())));
    assert!(enabled.iter().any(|registration| registration.service_jid
        == "other-push.example.com"
        && registration.node.as_deref() == Some(deleted.node())));
    assert_eq!(
        registrations
            .get_for_user("other-owner@example.com")
            .await
            .expect("unrelated owner untouched")
            .len(),
        1
    );
    let mut rows = outbox.query("SELECT status, attempt_count, next_attempt_at_ms, queue_acceptance_may_exist, last_error FROM notification_outbox WHERE job_id = ?", crate::db_params![gone.job_id.as_str()]).await.expect("gone job");
    let row = rows.next().await.expect("row").expect("job");
    assert_eq!(row.get::<String>(0).expect("status"), STATUS_FAILED);
    assert_eq!(row.get::<i64>(1).expect("attempts"), 0);
    assert!(row.get::<Option<i64>>(2).expect("retry deadline").is_none());
    assert_eq!(row.get::<i64>(3).expect("uncertainty"), 0);
    assert_eq!(row.get::<String>(4).expect("reason"), "registration_gone");
    drop(rows);
    assert!(push
        .queued_publish_jobs()
        .await
        .expect("no gone provider work")
        .is_empty());
    assert!(matches!(
        outbox
            .publish_claimed_job(live, &push, &registrations, &inbox, &blocking, &service)
            .await
            .expect("healthy outcome"),
        NotificationOutboxPublishOutcome::Published { .. }
    ));
    assert_eq!(
        push.queued_publish_jobs()
            .await
            .expect("healthy provider work")
            .len(),
        1
    );
    assert_eq!(
        metrics
            .counter_sum("xmpp.push.outbox_dead_lettered", &[])
            .unwrap_or(0),
        0
    );
    assert_eq!(
        metrics
            .counter_sum("xmpp.push.outbox_retry_scheduled", &[])
            .unwrap_or(0),
        0
    );
    drop(outbox);
    drop(registrations);
    drop(push);
    fixture.close().await;
}

async fn gone_registration_preserves_prior_provider_custody(
    fixture: IngressFixture,
    host_registration_missing: bool,
) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let outbox = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("outbox");
    let registrations = DatabasePushRegistrationStore::try_new(fixture.db.clone())
        .await
        .expect("registrations");
    let notification = candidate("gone-after-unknown-acceptance");
    let owner = notification.recipient_bare_jid();
    let service: BareJid = "push.example.com".parse().expect("service");
    let push = configured_push(&fixture, &service).await;
    let node = push
        .ensure_node(owner, FCM_TEST_APP_ID)
        .await
        .expect("node");
    push.upsert_device(owner, fcm_registration("prior-device", node.node()))
        .await
        .expect("device");
    push.register_first_party_node_for_owner(owner, service.as_str(), node.node(), None)
        .await
        .expect("registration");
    enqueue_jobs_for_test(&outbox, &notification, &[target_named(node.node())]).await;
    let claim = outbox
        .claim_due_outbox_jobs(1)
        .await
        .expect("first claim")
        .remove(0);
    attach_outbox_custody(&fixture, &notification, &claim).await;
    let inbox = inbox_with_unread(owner, notification.conversation_jid(), 1).await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    assert!(matches!(
        outbox
            .publish_claimed_job(&claim, &push, &registrations, &inbox, &blocking, &service)
            .await
            .expect("initial acceptance"),
        NotificationOutboxPublishOutcome::Published { .. }
    ));
    // Model a committed provider acceptance whose reply was lost upstream,
    // followed by node deactivation that did not authoritatively cancel custody.
    fixture.execute("UPDATE notification_outbox SET status = 'queued', published_at_ms = NULL, next_attempt_at_ms = 1 WHERE job_id = ?", crate::db_params![claim.job_id.as_str()]).await;
    fixture
        .execute(
            "UPDATE push_publish_jobs SET upstream_completed = 0 WHERE ancestry_job_id = ?",
            crate::db_params![claim.job_id.as_str()],
        )
        .await;
    if host_registration_missing {
        fixture.execute("DELETE FROM push_registrations WHERE owner_bare_jid = ? AND push_service_jid = ? AND node = ?", crate::db_params![owner.to_string(), service.to_string(), node.node()]).await;
    } else {
        fixture
            .execute(
                "UPDATE push_nodes SET status = 'disabled' WHERE node = ?",
                crate::db_params![node.node()],
            )
            .await;
    }
    let retry = outbox
        .claim_due_outbox_jobs(1)
        .await
        .expect("uncertain claim")
        .remove(0);
    assert!(retry.queue_acceptance_may_exist);
    let approved_payload = retry.approved_payload.clone().expect("frozen payload");
    assert!(matches!(
        outbox
            .publish_claimed_job(&retry, &push, &registrations, &inbox, &blocking, &service)
            .await
            .expect("gone outcome"),
        NotificationOutboxPublishOutcome::Failed { .. }
    ));
    assert!(registrations
        .get_for_user(&owner.to_string())
        .await
        .expect("disabled target")
        .is_empty());
    assert_eq!(fixture.count("notification_outbox WHERE status = 'failed' AND attempt_count = 0 AND queue_acceptance_may_exist = 1 AND claim_token IS NULL AND next_attempt_at_ms IS NULL").await, 1);
    let mut rows = outbox
        .query(
            "SELECT approved_payload_xml FROM notification_outbox WHERE job_id = ?",
            crate::db_params![retry.job_id.as_str()],
        )
        .await
        .expect("frozen job");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("job")
            .get::<String>(0)
            .expect("payload"),
        String::from(&approved_payload)
    );
    drop(rows);
    assert_eq!(fixture.count("push_publish_jobs WHERE status = 'queued' AND upstream_completed = 0 AND ancestry_job_id IS NOT NULL").await, 1);
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
    assert_eq!(
        outbox
            .prune_completed_before(i64::MAX, 16)
            .await
            .expect("retain custody")
            .jobs_deleted,
        0
    );
    assert!(outbox
        .drain_due_outbox_jobs(&push, &registrations, &inbox, &blocking, &service, 16)
        .await
        .expect("parked jobs only reconcile")
        .is_empty());
    assert_eq!(
        metrics
            .counter_sum("xmpp.push.outbox_dead_lettered", &[])
            .unwrap_or(0),
        0
    );
    assert_eq!(
        metrics
            .counter_sum("xmpp.push.outbox_retry_scheduled", &[])
            .unwrap_or(0),
        0
    );
    assert_eq!(
        metrics.counter_sum("xmpp.push.disposition", &[("disposition", "parked")]),
        Some(1)
    );
    drop(outbox);
    drop(registrations);
    drop(push);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_xep0357_deleted_node_disables_only_its_registration() {
    deleted_node_disables_only_its_registration(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_xep0357_deleted_node_disables_only_its_registration() {
    if let Some(fixture) = IngressFixture::postgres("gone_registration").await {
        deleted_node_disables_only_its_registration(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_xep0357_gone_registration_preserves_prior_provider_custody() {
    gone_registration_preserves_prior_provider_custody(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn postgres_xep0357_gone_registration_preserves_prior_provider_custody() {
    if let Some(fixture) = IngressFixture::postgres("gone_registration_custody").await {
        gone_registration_preserves_prior_provider_custody(fixture, false).await;
    }
}

#[tokio::test]
async fn sqlite_xep0357_missing_registration_parks_prior_provider_custody() {
    gone_registration_preserves_prior_provider_custody(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn postgres_xep0357_missing_registration_parks_prior_provider_custody() {
    if let Some(fixture) = IngressFixture::postgres("missing_registration_custody").await {
        gone_registration_preserves_prior_provider_custody(fixture, true).await;
    }
}

#[tokio::test]
async fn postgres_xep0357_delayed_gone_outcome_preserves_later_reenable() {
    let Some(fixture) = IngressFixture::postgres("gone_registration_reenable").await else {
        return;
    };
    let outbox = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("outbox");
    let registrations = DatabasePushRegistrationStore::try_new(fixture.db.clone())
        .await
        .expect("registrations");
    let service: BareJid = "push.example.com".parse().expect("service");
    let push = configured_push(&fixture, &service).await;
    let notification = candidate("gone-before-reenable");
    let owner = notification.recipient_bare_jid();
    let node = push
        .ensure_node(owner, FCM_TEST_APP_ID)
        .await
        .expect("node");
    push.upsert_device(owner, fcm_registration("reenabled-device", node.node()))
        .await
        .expect("device");
    push.register_first_party_node_for_owner(owner, service.as_str(), node.node(), None)
        .await
        .expect("registration");
    fixture
        .execute(
            "UPDATE push_nodes SET status = 'disabled' WHERE node = ?",
            crate::db_params![node.node()],
        )
        .await;
    enqueue_jobs_for_test(&outbox, &notification, &[target_named(node.node())]).await;
    let claim = outbox
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    // Hold the outbox's failed transition after admission committed its cleanup.
    // Re-enabling must win before that old callback is allowed to finish.
    fixture.execute("CREATE FUNCTION gate_gone_outcome() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(7192014); RETURN NEW; END $$", ()).await;
    fixture.execute("CREATE TRIGGER gate_gone_outcome BEFORE UPDATE ON notification_outbox FOR EACH ROW WHEN (NEW.status = 'failed' AND NEW.last_error = 'registration_gone') EXECUTE FUNCTION gate_gone_outcome()", ()).await;
    let mut gate = fixture.db.begin().await.expect("gate transaction");
    gate.execute("SELECT pg_advisory_xact_lock(7192014)", ())
        .await
        .expect("hold gate");
    let publishing = {
        let outbox = outbox.clone();
        let push = push.clone();
        let registrations = registrations.clone();
        let service = service.clone();
        let inbox = inbox_with_unread(owner, notification.conversation_jid(), 1).await;
        tokio::spawn(async move {
            outbox
                .publish_claimed_job(
                    &claim,
                    &push,
                    &registrations,
                    &inbox,
                    &waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new(),
                    &service,
                )
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if fixture
                .count("pg_locks WHERE locktype = 'advisory' AND objid = 7192014 AND NOT granted")
                .await
                > 0
            {
                break;
            }
            assert!(
                !publishing.is_finished(),
                "publication must reach the transition gate"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("admission reached gone transition");
    assert!(registrations
        .get_for_user(&owner.to_string())
        .await
        .expect("cleanup committed")
        .is_empty());
    assert_eq!(
        push.ensure_node(owner, FCM_TEST_APP_ID)
            .await
            .expect("reactivated node")
            .node(),
        node.node()
    );
    push.register_first_party_node_for_owner(owner, service.as_str(), node.node(), None)
        .await
        .expect("fresh enable");
    gate.commit().await.expect("release old callback");
    assert!(matches!(
        publishing.await.expect("publisher").expect("outcome"),
        NotificationOutboxPublishOutcome::Failed { .. }
    ));
    assert_eq!(
        registrations
            .get_for_user(&owner.to_string())
            .await
            .expect("fresh enable survives")
            .len(),
        1
    );
    let fresh_notification = candidate("after-reenable");
    enqueue_jobs_for_test(&outbox, &fresh_notification, &[target_named(node.node())]).await;
    let fresh = outbox
        .claim_due_outbox_jobs(1)
        .await
        .expect("fresh claim")
        .remove(0);
    let inbox = inbox_with_unread(owner, fresh_notification.conversation_jid(), 1).await;
    assert!(matches!(
        outbox
            .publish_claimed_job(
                &fresh,
                &push,
                &registrations,
                &inbox,
                &waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new(),
                &service
            )
            .await
            .expect("fresh publication"),
        NotificationOutboxPublishOutcome::Published { .. }
    ));
    drop(outbox);
    drop(registrations);
    drop(push);
    fixture.close().await;
}
