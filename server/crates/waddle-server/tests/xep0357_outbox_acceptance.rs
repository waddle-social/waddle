//! XEP-0357: a frozen preview does not prove durable queue acceptance.
pub mod ingress_support;

use ingress_support::IngressFixture;
use jid::BareJid;
use minidom::Element;
use std::sync::Arc;
use uuid::Uuid;
use waddle_server::{
    notification_outbox::{
        NotificationOutboxJob, NotificationOutboxPublishOutcome, NotificationOutboxStore,
        WADDLE_PUSH_CONTEXT_NS,
    },
    push_service::{DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration},
};
use waddle_xmpp::{
    inbox::{
        storage::{InMemoryInboxStorage, InboxStorage},
        ConversationKind, InboxEntry,
    },
    pubsub::{Affiliation, InMemoryPubSubStorage, PubSubStorage},
    push::{InMemoryPushStore, PushSubscription, PushSubscriptionStore},
    xep::xep0191::InMemoryBlockingStorage,
};

fn publish_options(access_model: &str) -> Element {
    use waddle_xmpp::xep::{NS_DATA_FORMS, NS_PUBSUB_PUBLISH_OPTIONS};
    let field = |name: &str, value: &str| {
        Element::builder("field", NS_DATA_FORMS)
            .attr(minidom::rxml::xml_ncname!("var").to_owned(), name)
            .append(
                Element::builder("value", NS_DATA_FORMS)
                    .append(value)
                    .build(),
            )
            .build()
    };
    Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(field("FORM_TYPE", NS_PUBSUB_PUBLISH_OPTIONS))
        .append(field("pubsub#access_model", access_model))
        .build()
}

struct Fixture {
    outbox_db: IngressFixture,
    push_db: IngressFixture,
    outbox: NotificationOutboxStore,
    push: DatabasePushServiceStore,
    backing: Arc<InMemoryPubSubStorage>,
    subscriptions: InMemoryPushStore,
    inbox: InMemoryInboxStorage,
    owner: BareJid,
    service: BareJid,
    node: String,
    job_id: Uuid,
}

impl Fixture {
    async fn new(outbox_db: IngressFixture, push_db: IngressFixture) -> Self {
        let owner: BareJid = "alice@example.com".parse().expect("owner");
        let service: BareJid = "push.example.com".parse().expect("service");
        let outbox = NotificationOutboxStore::new(outbox_db.db.clone())
            .await
            .expect("outbox");
        let backing = Arc::new(InMemoryPubSubStorage::new());
        let push = DatabasePushServiceStore::new_with_secret_key_and_pubsub(
            push_db.db.clone(),
            &rand::random::<[u8; 32]>(),
            service.clone(),
            backing.clone(),
        )
        .await
        .expect("push service");
        waddle_server::push_registrations::DatabasePushRegistrationStore::new(push_db.db.clone())
            .await
            .expect("registration schema");
        let node = push
            .ensure_node(&owner, "web")
            .await
            .expect("node")
            .node()
            .to_owned();
        push.upsert_device(
            &owner,
            PushDeviceRegistration::new("fixture-device", &node, PushDevicePlatform::Fcm, "test"),
        )
        .await
        .expect("device");
        let options = publish_options("whitelist");
        push.register_first_party_node_for_owner(&owner, service.as_str(), &node, Some(&options))
            .await
            .expect("service registration");
        let subscriptions = InMemoryPushStore::new();
        subscriptions
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: service.to_string(),
                node: Some(node.clone()),
                publish_options: Some(publish_options("whitelist")),
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("subscription");
        let inbox = InMemoryInboxStorage::new();
        inbox
            .upsert(
                &owner,
                InboxEntry::new(
                    "bob@example.com".parse().expect("partner"),
                    ConversationKind::Direct,
                    "archive",
                    1,
                ),
                true,
            )
            .await
            .expect("unread");
        let job_id = Uuid::new_v4();
        let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
            .attr(
                minidom::rxml::xml_ncname!("conversation").to_owned(),
                "bob@example.com",
            )
            .attr(minidom::rxml::xml_ncname!("class").to_owned(), "dm")
            .attr(
                minidom::rxml::xml_ncname!("stanza-id").to_owned(),
                "archive",
            )
            .build();
        outbox_db.execute(
            "INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, status, created_at_ms, updated_at_ms, queue_acceptance_may_exist) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, 1, 1, 0)",
            waddle_server::db_params![job_id.to_string(), owner.to_string(), service.to_string(), node.clone(), "bob@example.com", "bob@example.com/phone", "[\"bob@example.com/phone\"]", "dm", String::from(&context), "queued"],
        ).await;
        Self {
            outbox_db,
            push_db,
            outbox,
            push,
            backing,
            subscriptions,
            inbox,
            owner,
            service,
            node,
            job_id,
        }
    }

    async fn claim(&self) -> NotificationOutboxJob {
        self.outbox_db
            .execute(
                "UPDATE notification_outbox SET next_attempt_at_ms = NULL WHERE job_id = ?",
                waddle_server::db_params![self.job_id.to_string()],
            )
            .await;
        self.outbox
            .claim_due_outbox_jobs(1)
            .await
            .expect("claim")
            .remove(0)
    }

    async fn publish(&self, job: &NotificationOutboxJob) -> NotificationOutboxPublishOutcome {
        self.outbox
            .publish_claimed_job(
                job,
                &self.push,
                &self.subscriptions,
                &self.inbox,
                &InMemoryBlockingStorage::new(),
                &self.service,
            )
            .await
            .expect("publish outcome")
    }

    async fn inactive(&self) {
        self.push_db
            .execute(
                "UPDATE push_nodes SET status = ? WHERE node = ?",
                waddle_server::db_params!["disabled", self.node.clone()],
            )
            .await;
    }

    async fn full(&self) {
        let payload = Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build();
        self.push_db.execute("WITH RECURSIVE items(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM items WHERE n < 10000) INSERT INTO push_publish_jobs (job_id, owner_bare_jid, push_service_jid, node, item_id, payload_xml, status, created_at_ms, updated_at_ms) SELECT CAST(n AS TEXT), ?, ?, ?, CAST(n AS TEXT), ?, 'queued', 1, 1 FROM items", waddle_server::db_params![self.owner.to_string(), self.service.to_string(), self.node.clone(), String::from(&payload)]).await;
    }

    async fn close(self) {
        drop(self.outbox);
        drop(self.push);
        self.outbox_db.close().await;
        self.push_db.close().await;
    }
}

async fn known_refusal_reaches_attempt_limit(fixture: Fixture, full: bool) {
    if full {
        fixture.full().await;
    } else {
        fixture.inactive().await;
    }
    for attempt in 1..=5 {
        let job = fixture.claim().await;
        let outcome = fixture.publish(&job).await;
        if attempt < 5 {
            assert!(matches!(
                outcome,
                NotificationOutboxPublishOutcome::RetryScheduled { .. }
            ));
        } else {
            assert!(
                matches!(outcome, NotificationOutboxPublishOutcome::Failed { .. }),
                "five definite queue refusals must terminally fail: {outcome:?}"
            );
        }
    }
    assert_eq!(
        fixture
            .outbox_db
            .count("notification_outbox WHERE status = 'failed' AND attempt_count = 5")
            .await,
        1
    );
    assert_eq!(
        fixture
            .push_db
            .count("push_publish_jobs WHERE acceptance_scope = 'canonical'")
            .await,
        0
    );
    fixture.close().await;
}

async fn known_refusal_can_still_suppress_unread_zero(fixture: Fixture) {
    fixture.inactive().await;
    let first = fixture.claim().await;
    assert!(matches!(
        fixture.publish(&first).await,
        NotificationOutboxPublishOutcome::RetryScheduled { .. }
    ));
    assert_eq!(fixture.push_db.count("push_publish_jobs").await, 0);
    fixture
        .inbox
        .mark_read(
            &fixture.owner,
            &"bob@example.com".parse().expect("partner"),
            None,
        )
        .await
        .expect("read");
    let job = fixture.claim().await;
    assert!(matches!(fixture.publish(&job).await, NotificationOutboxPublishOutcome::Suppressed { .. }), "a frozen preview never accepted by the queue may be suppressed after the recipient reads it");
    assert_eq!(fixture.outbox_db.count("notification_outbox").await, 0);
    assert_eq!(fixture.push_db.count("push_publish_jobs").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_queue_full_refusals_reach_attempt_limit() {
    known_refusal_reaches_attempt_limit(
        Fixture::new(
            IngressFixture::sqlite().await,
            IngressFixture::sqlite().await,
        )
        .await,
        true,
    )
    .await;
}
#[tokio::test]
async fn sqlite_inactive_node_refusals_reach_attempt_limit() {
    known_refusal_reaches_attempt_limit(
        Fixture::new(
            IngressFixture::sqlite().await,
            IngressFixture::sqlite().await,
        )
        .await,
        false,
    )
    .await;
}
#[tokio::test]
async fn sqlite_frozen_unaccepted_preview_is_suppressed_when_read() {
    known_refusal_can_still_suppress_unread_zero(
        Fixture::new(
            IngressFixture::sqlite().await,
            IngressFixture::sqlite().await,
        )
        .await,
    )
    .await;
}
#[tokio::test]
async fn postgres_queue_full_refusals_reach_attempt_limit() {
    let Some(outbox) = IngressFixture::postgres("queue_refusal_outbox").await else {
        return;
    };
    let Some(push) = IngressFixture::postgres("queue_refusal_push").await else {
        return;
    };
    known_refusal_reaches_attempt_limit(Fixture::new(outbox, push).await, true).await;
}
#[tokio::test]
async fn postgres_inactive_node_refusals_reach_attempt_limit() {
    let Some(outbox) = IngressFixture::postgres("inactive_refusal_outbox").await else {
        return;
    };
    let Some(push) = IngressFixture::postgres("inactive_refusal_push").await else {
        return;
    };
    known_refusal_reaches_attempt_limit(Fixture::new(outbox, push).await, false).await;
}
#[tokio::test]
async fn postgres_frozen_unaccepted_preview_is_suppressed_when_read() {
    let Some(outbox) = IngressFixture::postgres("read_refusal_outbox").await else {
        return;
    };
    let Some(push) = IngressFixture::postgres("read_refusal_push").await else {
        return;
    };
    known_refusal_can_still_suppress_unread_zero(Fixture::new(outbox, push).await).await;
}

async fn queue_commit_followed_by_backing_failure_keeps_uncertainty(fixture: Fixture) {
    fixture
        .backing
        .set_affiliation(
            &fixture.service,
            &fixture.node,
            &fixture.owner,
            Affiliation::None,
        )
        .await
        .expect("remove backing affiliation");
    let first = fixture.claim().await;
    assert!(matches!(
        fixture.publish(&first).await,
        NotificationOutboxPublishOutcome::RetryScheduled { .. }
    ));
    assert_eq!(
        fixture
            .push_db
            .count("push_publish_jobs WHERE acceptance_scope = 'canonical'")
            .await,
        1,
        "backing projection failed after queue commit"
    );
    let accepted = fixture
        .outbox
        .pending_outbox_jobs()
        .await
        .expect("frozen delivery")
        .remove(0)
        .to_xep0357_pubsub_item();
    fixture.inactive().await;
    fixture
        .inbox
        .mark_read(
            &fixture.owner,
            &"bob@example.com".parse().expect("partner"),
            None,
        )
        .await
        .expect("read");
    for _ in 0..6 {
        let job = fixture.claim().await;
        assert!(matches!(fixture.publish(&job).await, NotificationOutboxPublishOutcome::RetryScheduled { .. }), "later known refusal cannot erase prior accepted work or suppress read-zero uncertainty");
    }
    assert_eq!(fixture.outbox_db.count("notification_outbox WHERE status = 'queued' AND attempt_count = 7 AND queue_acceptance_may_exist = 1").await, 1);
    fixture
        .push_db
        .execute(
            "UPDATE push_nodes SET status = ? WHERE node = ?",
            waddle_server::db_params!["active", fixture.node.clone()],
        )
        .await;
    fixture
        .backing
        .set_affiliation(
            &fixture.service,
            &fixture.node,
            &fixture.owner,
            Affiliation::PublishOnly,
        )
        .await
        .expect("restore backing affiliation");
    fixture
        .subscriptions
        .register(PushSubscription {
            user_jid: fixture.owner.to_string(),
            service_jid: fixture.service.to_string(),
            node: Some(fixture.node.clone()),
            publish_options: Some(publish_options("open")),
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
        .expect("changed registration options");
    let replay = fixture.claim().await;
    let replay_item = replay.to_xep0357_pubsub_item();
    assert_eq!(
        replay_item.id, accepted.id,
        "unknown retry preserves the same key across distinct databases"
    );
    assert_eq!(
        replay_item.payload, accepted.payload,
        "unknown retry preserves the same frozen payload across distinct databases"
    );
    assert!(matches!(
        fixture.publish(&replay).await,
        NotificationOutboxPublishOutcome::Published { .. }
    ));
    assert_eq!(
        fixture
            .push_db
            .count("push_publish_jobs WHERE acceptance_scope = 'canonical'")
            .await,
        1
    );
    assert_eq!(
        fixture.push_db.count("push_delivery_attempts").await,
        0,
        "queue acceptance is not provider acceptance"
    );
    assert_eq!(fixture.push_db.optional_text("SELECT publish_options_xml FROM push_publish_jobs WHERE acceptance_scope = 'canonical'").await, Some(String::from(&publish_options("whitelist"))), "later registration changes cannot alter frozen publish options");
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_queue_commit_survives_backing_failure_and_later_refusal() {
    queue_commit_followed_by_backing_failure_keeps_uncertainty(
        Fixture::new(
            IngressFixture::sqlite().await,
            IngressFixture::sqlite().await,
        )
        .await,
    )
    .await;
}
#[tokio::test]
async fn postgres_queue_commit_survives_backing_failure_and_later_refusal() {
    let Some(outbox) = IngressFixture::postgres("uncertain_outbox").await else {
        return;
    };
    let Some(push) = IngressFixture::postgres("uncertain_push").await else {
        return;
    };
    queue_commit_followed_by_backing_failure_keeps_uncertainty(Fixture::new(outbox, push).await)
        .await;
}
