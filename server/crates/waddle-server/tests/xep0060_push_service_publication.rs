//! XEP-0060 §7.1 publication semantics for the XEP-0357 Push Service profile.
//!
//! A repeated ItemID replaces the backing item while accepting fresh frozen
//! provider work. PubSub success is distinct from downstream provider acceptance.

use std::sync::Arc;

use jid::BareJid;
use minidom::Element;
use waddle_server::db::Database;
use waddle_server::db_params;
use waddle_server::pubsub::DatabasePubSubStorage;
use waddle_server::push_registrations::DatabasePushRegistrationStore;
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_xmpp::pubsub::{PubSubItem, PubSubStorage};
use waddle_xmpp::xep::xep0357::NS_PUSH;

#[tokio::test]
async fn same_item_id_republication_replaces_backing_without_provider_acceptance() {
    assert_same_item_id_republication(PushDevicePlatform::Web).await;
}

#[tokio::test]
async fn same_item_id_republication_after_terminal_work_accepts_fresh_job() {
    // Retain the original regression's terminal first publication: its
    // device-success marker must not suppress a new wire publication.
    assert_same_item_id_republication(PushDevicePlatform::Fcm).await;
}

async fn assert_same_item_id_republication(platform: PushDevicePlatform) {
    let (expected_job_status, expected_attempt_status) = match platform {
        PushDevicePlatform::Web => ("queued", "web-not-configured"),
        PushDevicePlatform::Fcm => ("published", "fake-sent"),
        PushDevicePlatform::Apns => panic!("fixture covers unavailable Web Push and stubbed FCM"),
    };
    let db = Database::in_memory("xep0060-push-publication")
        .await
        .expect("queue database");
    DatabasePushRegistrationStore::new(db.clone())
        .await
        .expect("registrations");
    let backing = Arc::new(
        DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("independent PubSub backing"),
    );
    let service: BareJid = "push.example.com".parse().expect("push service");
    let store = DatabasePushServiceStore::new_with_secret_key_and_pubsub(
        db.clone(),
        &rand::random::<[u8; 32]>(),
        service.clone(),
        backing.clone(),
    )
    .await
    .expect("push store");
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let node = store.ensure_node(&owner, "wire-item").await.expect("node");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("device", node.node(), platform, "test"),
        )
        .await
        .expect("device");
    store
        .register_first_party_node_for_owner(&owner, service.as_str(), node.node(), None)
        .await
        .expect("registration");
    let original = PubSubItem::new(
        Some("current".to_string()),
        Some(Element::builder("notification", NS_PUSH).build()),
    );
    let replacement = PubSubItem::new(
        original.id.clone(),
        Some(
            Element::builder("notification", NS_PUSH)
                .append(
                    Element::builder(
                        "context",
                        waddle_server::notification_outbox::WADDLE_PUSH_CONTEXT_NS,
                    )
                    .attr(
                        minidom::rxml::xml_ncname!("stanza-id").to_owned(),
                        "replacement",
                    )
                    .build(),
                )
                .build(),
        ),
    );
    for item in [&original, &replacement] {
        let result = store
            .publish_registered_notification_from_user_server_with_publish_options(
                service.as_str(),
                node.node(),
                item,
                &owner,
                None,
            )
            .await
            .expect("XEP-0060 permits publishing and replacing the same ItemID");
        assert_eq!(
            result.item_id(),
            "current",
            "the supplied wire ItemID is stable"
        );
        assert_eq!(result.attempted_devices(), 1);
        let stored = backing
            .get_items(&service, node.node(), None, &[])
            .await
            .expect("backing item");
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].id,
            result.item_id(),
            "backing keeps the returned wire ItemID"
        );
        assert_eq!(
            stored[0].to_pubsub_item().payload,
            item.payload,
            "republication overwrites the backing payload"
        );
    }
    let queued = store.queued_publish_jobs().await.expect("provider queue");
    assert_eq!(
        queued.len(),
        if platform == PushDevicePlatform::Web {
            2
        } else {
            0
        },
        "unavailable provider work stays queued even after PubSub success"
    );
    assert!(queued
        .iter()
        .all(|job| job.item_id() == "current" && job.status() == "queued"));
    let conn = db.guard().await.expect("guard");
    let mut rows = conn.query(
        "SELECT payload_xml, ancestry_job_id, backing_state, published_at_ms, job_id, status FROM push_publish_jobs WHERE node = ? AND item_id = ?",
        db_params![node.node(), "current"],
    ).await.expect("acceptances");
    let mut payloads = Vec::new();
    let mut job_ids = Vec::new();
    while let Some(row) = rows.next().await.expect("row") {
        payloads.push(row.get::<String>(0).expect("frozen payload"));
        assert_eq!(
            row.get::<Option<String>>(1).expect("canonical binding"),
            None
        );
        assert_eq!(row.get::<String>(2).expect("backing state"), "published");
        assert_eq!(
            row.get::<Option<i64>>(3)
                .expect("job completion time")
                .is_some(),
            platform == PushDevicePlatform::Fcm
        );
        job_ids.push(row.get::<String>(4).expect("job identity"));
        assert_eq!(
            row.get::<String>(5).expect("job status"),
            expected_job_status
        );
    }
    assert_eq!(payloads.len(), 2);
    assert_ne!(
        job_ids[0], job_ids[1],
        "each wire publication accepts fresh provider work"
    );
    assert!(payloads.contains(&String::from(original.payload.as_ref().expect("original"))));
    assert!(payloads.contains(&String::from(
        replacement.payload.as_ref().expect("replacement")
    )));
    drop(rows);
    drop(conn);
    let attempts = store
        .delivery_attempts_for_node(node.node())
        .await
        .expect("attempts");
    assert_eq!(attempts.len(), 2);
    assert!(
        attempts
            .iter()
            .all(|attempt| attempt.item_id() == "current"
                && attempt.status() == expected_attempt_status),
        "successful PubSub publication and queue acceptance do not fabricate a provider receipt"
    );
}
