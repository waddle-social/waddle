//! XEP-0060 §7.1 publication semantics for the XEP-0357 Push Service profile.
//!
//! A repeated ItemID replaces the backing item while accepting fresh frozen
//! provider work. PubSub success is distinct from downstream provider acceptance.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jid::BareJid;
use minidom::Element;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use waddle_server::db::Database;
use waddle_server::db_params;
use waddle_server::pubsub::DatabasePubSubStorage;
use waddle_server::push_registrations::DatabasePushRegistrationStore;
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_xmpp::pubsub::{PubSubItem, PubSubStorage};
use waddle_xmpp::push::fcm::{
    FcmAccessToken, FcmAccessTokenSource, FcmAppId, FcmOutcome, FcmProjectId, FcmRequest,
    FcmSender, FcmTokenError,
};
use waddle_xmpp::xep::xep0357::NS_PUSH;

/// Android application id the fake FCM provider serves.
const FCM_APP_ID: &str = "social.waddle.android";

/// FCM transport that accepts every message.
struct AcceptingFcmSender;

impl FcmSender for AcceptingFcmSender {
    fn send(
        &self,
        _request: FcmRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = FcmOutcome> + Send + '_>> {
        Box::pin(async { FcmOutcome::Sent })
    }
}

struct StaticFcmTokens;

impl FcmAccessTokenSource for StaticFcmTokens {
    fn current(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<FcmAccessToken, FcmTokenError>> + Send + '_>> {
        Box::pin(async { Ok(FcmAccessToken::new("ya29.test".to_string())) })
    }

    fn invalidate(&self, _rejected: &FcmAccessToken) {}
}

fn device(platform: PushDevicePlatform, node: &str) -> PushDeviceRegistration {
    let registration = PushDeviceRegistration::new("device", node, platform, "prod");
    if platform != PushDevicePlatform::Fcm {
        return registration;
    }
    let secret = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
    let auth: [u8; 16] = rand::random();
    registration
        .with_provider_token(Some("fcm-token-device".to_string()))
        .with_provider_key_material(Some(
            URL_SAFE_NO_PAD.encode(secret.public_key().to_encoded_point(false).as_bytes()),
        ))
        .with_provider_auth(Some(URL_SAFE_NO_PAD.encode(auth)))
}

/// XEP-0357 notification whose Waddle context identifies `stanza_id`.
fn notification(stanza_id: &str) -> Element {
    Element::builder("notification", NS_PUSH)
        .append(
            Element::builder(
                "context",
                waddle_server::notification_outbox::WADDLE_PUSH_CONTEXT_NS,
            )
            .attr(
                minidom::rxml::xml_ncname!("conversation").to_owned(),
                "bob@example.com",
            )
            .attr(minidom::rxml::xml_ncname!("class").to_owned(), "dm")
            .attr(
                minidom::rxml::xml_ncname!("stanza-id").to_owned(),
                stanza_id,
            )
            .build(),
        )
        .build()
}

#[tokio::test]
async fn same_item_id_republication_replaces_backing_without_provider_acceptance() {
    assert_same_item_id_republication(PushDevicePlatform::Web).await;
}

#[tokio::test]
async fn same_item_id_republication_after_terminal_work_accepts_fresh_job() {
    // Retain the original regression's terminal first publication: its
    // device-success marker (a fake FCM acceptance) must not suppress a
    // new wire publication.
    assert_same_item_id_republication(PushDevicePlatform::Fcm).await;
}

async fn assert_same_item_id_republication(platform: PushDevicePlatform) {
    let (expected_job_status, expected_attempt_status) = match platform {
        PushDevicePlatform::Web => ("queued", "web-not-configured"),
        PushDevicePlatform::Fcm => ("published", "fcm-delivered"),
        PushDevicePlatform::Apns => panic!("fixture covers unavailable Web Push and fake FCM"),
    };
    let db = Database::in_memory("xep0060-push-publication")
        .await
        .expect("queue database");
    DatabasePushRegistrationStore::try_new(db.clone())
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
    .expect("push store")
    .with_fcm_provider(
        Arc::new(StaticFcmTokens),
        Arc::new(AcceptingFcmSender),
        FcmProjectId::parse("waddle-test").expect("project id"),
        FcmAppId::parse(FCM_APP_ID).expect("app id"),
    );
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let node = store.ensure_node(&owner, FCM_APP_ID).await.expect("node");
    store
        .upsert_device(&owner, device(platform, node.node()))
        .await
        .expect("device");
    store
        .register_first_party_node_for_owner(&owner, service.as_str(), node.node(), None)
        .await
        .expect("registration");
    let original = PubSubItem::new(Some("current".to_string()), Some(notification("original")));
    let replacement = PubSubItem::new(original.id.clone(), Some(notification("replacement")));
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
