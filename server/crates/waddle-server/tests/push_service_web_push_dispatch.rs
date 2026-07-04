//! XEP-0357 §6 forward cleanup and Web Push dispatch outcome tests.
//!
//! When the Web Push relay reports the subscription is permanently
//! gone (404/410), the publish-job worker records a `web-gone`
//! attempt AND marks the underlying `push_devices` row disabled in
//! the same tx so future publish jobs for the node skip it. These
//! tests exercise the full path with a mock `WebPushSender` so the
//! cleanup behavior is locked in without needing a live relay.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use jid::BareJid;
use minidom::Element;
use waddle_server::db::{Database, IntoParams, Rows};
use waddle_server::db_params;
use waddle_server::push_service::vapid_storage::VapidStorage;
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::push::types::{VapidSub, WebPushOutcome};
use waddle_xmpp::push::{WebPushRequest, WebPushSender};
use waddle_xmpp::xep::xep0357::NS_PUSH;

/// Mirrors `DEVICE_STATUS_ACTIVE` in `waddle_server::push_service`.
const DEVICE_STATUS_ACTIVE: &str = "active";
/// Mirrors `DEVICE_STATUS_DISABLED` in `waddle_server::push_service`.
const DEVICE_STATUS_DISABLED: &str = "disabled";
/// Mirrors `PUBLISH_JOB_STATUS_FAILED` in `waddle_server::push_service`.
const PUBLISH_JOB_STATUS_FAILED: &str = "failed";
/// Mirrors `crate::push_service::dispatch::ATTEMPT_STATUS_WEB_GONE`.
const ATTEMPT_STATUS_WEB_GONE: &str = "web-gone";
/// Mirrors `crate::push_service::dispatch::ATTEMPT_STATUS_WEB_DELIVERED`.
const ATTEMPT_STATUS_WEB_DELIVERED: &str = "web-delivered";

async fn store() -> DatabasePushServiceStore {
    DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("push-service")
            .await
            .expect("push service db"),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push service store")
}

fn owner() -> BareJid {
    "alice@example.com".parse().expect("owner jid")
}

async fn query(store: &DatabasePushServiceStore, sql: &str, params: impl IntoParams) -> Rows {
    let db = store.database();
    let conn = db.guard().await.expect("db guard");
    conn.query(sql, params).await.expect("query")
}

/// A `WebPushSender` that returns the same configured outcome for
/// every request. Counts calls so tests can assert the worker
/// actually invoked it.
#[derive(Clone)]
struct FixedOutcomeSender {
    outcome: WebPushOutcome,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl FixedOutcomeSender {
    fn new(outcome: WebPushOutcome) -> Self {
        Self {
            outcome,
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl WebPushSender for FixedOutcomeSender {
    fn send(
        &self,
        _request: WebPushRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = WebPushOutcome> + Send + '_>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }
}

/// Build a Web Push-shape `<notification>` payload with the typed
/// `urn:waddle:push:context:0` element the chat publisher emits.
/// `class` is the db-form value (e.g. `"dm"`, `"personal_mention"`).
fn web_push_notification_item(
    item_id: &str,
    conversation: &str,
    class: &str,
    message_count: u32,
) -> PubSubItem {
    use waddle_xmpp::xep::xep0004::NS_DATA_FORMS;
    let summary = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("field", NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                .append(
                    Element::builder("value", NS_DATA_FORMS)
                        .append("urn:xmpp:push:summary")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", NS_DATA_FORMS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "message-count",
                )
                .append(
                    Element::builder("value", NS_DATA_FORMS)
                        .append(message_count.to_string())
                        .build(),
                )
                .build(),
        )
        .build();
    let context = Element::builder("context", "urn:waddle:push:context:0")
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            conversation,
        )
        .attr(minidom::rxml::xml_ncname!("class").to_owned(), class)
        .build();
    let notification = Element::builder("notification", NS_PUSH)
        .append(summary)
        .append(context)
        .build();
    PubSubItem::new(Some(item_id.to_string()), Some(notification))
}

/// Generate a real (p256dh, auth) subscription pair the
/// `SubscriptionKeys` parser will accept. The relay-side mock
/// sender ignores them, but the worker's encrypt + sign path runs
/// for real so the keys must be valid.
fn fresh_subscription_material() -> (String, String) {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use p256::elliptic_curve::rand_core::OsRng;
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    use rand::RngExt as _;
    let secret = p256::SecretKey::random(&mut OsRng);
    let p256dh_bytes = secret.public_key().to_encoded_point(false);
    let p256dh = URL_SAFE_NO_PAD.encode(p256dh_bytes.as_bytes());
    let mut auth = [0u8; 16];
    rand::rng().fill(&mut auth[..]);
    let auth = URL_SAFE_NO_PAD.encode(auth);
    (p256dh, auth)
}

/// Build a store wired with a real VAPID signer (from
/// `VapidStorage::load_or_provision` against a fresh in-memory db)
/// and a [`FixedOutcomeSender`].
async fn store_with_web_push_provider(
    outcome: WebPushOutcome,
) -> (DatabasePushServiceStore, FixedOutcomeSender) {
    let db = Database::in_memory("push-service-web-push")
        .await
        .expect("db");
    let store = DatabasePushServiceStore::new_with_secret_key(
        db.clone(),
        b"waddle-push-service-test-secret-key-32b",
    )
    .await
    .expect("store");
    let signer = VapidStorage::load_or_provision(db, b"root-key")
        .await
        .expect("VAPID signer");
    let sender = FixedOutcomeSender::new(outcome);
    let sender_arc: Arc<dyn WebPushSender> = Arc::new(sender.clone());
    let sub = VapidSub::default_for_domain("example.com").expect("vapid sub");
    let store = store.with_web_push_provider(signer, sender_arc, sub);
    (store, sender)
}

async fn device_status(store: &DatabasePushServiceStore, node: &str, device_id: &str) -> String {
    let mut rows = query(
        store,
        "SELECT status FROM push_devices WHERE node = ? AND device_id = ?",
        db_params![node, device_id],
    )
    .await;
    let row = rows
        .next()
        .await
        .expect("status query row")
        .expect("device row present");
    row.get::<String>(0).expect("status column")
}

#[tokio::test]
async fn subscription_gone_marks_device_disabled_per_xep0357_6() {
    let (store, sender) =
        store_with_web_push_provider(WebPushOutcome::SubscriptionGone { status: 410 }).await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    let (p256dh, auth) = fresh_subscription_material();
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                .with_provider_endpoint(Some(
                    "https://push.example.com/abc-subscription".to_string(),
                ))
                .with_provider_token(Some(auth))
                .with_provider_key_material(Some(p256dh)),
        )
        .await
        .expect("device");
    store
        .publish_notification_from_user_server(
            node.node(),
            &web_push_notification_item("gone-item-1", "alice@example.com", "dm", 1),
            &owner,
        )
        .await
        .expect("publish");
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("drain");

    assert_eq!(
        sender.call_count(),
        1,
        "real send must run for an active web device with full material"
    );
    assert_eq!(
        device_status(&store, node.node(), "web-1").await,
        DEVICE_STATUS_DISABLED,
        "XEP-0357 §6: a `web-gone` outcome must flip the underlying device to disabled"
    );
    let attempts = store
        .delivery_attempts_for_node(node.node())
        .await
        .expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status(), ATTEMPT_STATUS_WEB_GONE);
}

#[tokio::test]
async fn delivered_outcome_keeps_device_active() {
    let (store, sender) =
        store_with_web_push_provider(WebPushOutcome::Delivered { status: 201 }).await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    let (p256dh, auth) = fresh_subscription_material();
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                .with_provider_endpoint(Some(
                    "https://push.example.com/abc-subscription".to_string(),
                ))
                .with_provider_token(Some(auth))
                .with_provider_key_material(Some(p256dh)),
        )
        .await
        .expect("device");
    store
        .publish_notification_from_user_server(
            node.node(),
            &web_push_notification_item("delivered-item-1", "alice@example.com", "dm", 1),
            &owner,
        )
        .await
        .expect("publish");
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("drain");

    assert_eq!(sender.call_count(), 1);
    assert_eq!(
        device_status(&store, node.node(), "web-1").await,
        DEVICE_STATUS_ACTIVE,
        "Delivered outcomes must NOT disable the device"
    );
    let attempts = store
        .delivery_attempts_for_node(node.node())
        .await
        .expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status(), ATTEMPT_STATUS_WEB_DELIVERED);
}

#[tokio::test]
async fn transient_outcome_keeps_device_active_and_requeues() {
    let (store, sender) = store_with_web_push_provider(WebPushOutcome::Transient {
        kind: waddle_xmpp::push::types::TransientFailure::Network,
    })
    .await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    let (p256dh, auth) = fresh_subscription_material();
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                .with_provider_endpoint(Some(
                    "https://push.example.com/abc-subscription".to_string(),
                ))
                .with_provider_token(Some(auth))
                .with_provider_key_material(Some(p256dh)),
        )
        .await
        .expect("device");
    store
        .publish_notification_from_user_server(
            node.node(),
            &web_push_notification_item("transient-item-1", "alice@example.com", "dm", 1),
            &owner,
        )
        .await
        .expect("publish");
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("drain");

    assert_eq!(sender.call_count(), 1);
    assert_eq!(
        device_status(&store, node.node(), "web-1").await,
        DEVICE_STATUS_ACTIVE,
        "Transient outcomes must keep the device active — XEP-0357 §6 disable applies only to permanent failures"
    );
}

#[tokio::test]
async fn all_payload_too_large_marks_job_failed_not_published() {
    // 413 from every device means the padding/bucket-class is
    // wrong (encoder/config bug), not a per-device problem.
    // Monitoring on `push_publish_jobs.status` must see FAILED,
    // not PUBLISHED — otherwise the regression hides behind a
    // "successful" status.
    let (store, sender) =
        store_with_web_push_provider(WebPushOutcome::PayloadTooLarge { status: 413 }).await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    let (p256dh, auth) = fresh_subscription_material();
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                .with_provider_endpoint(Some(
                    "https://push.example.com/abc-subscription".to_string(),
                ))
                .with_provider_token(Some(auth))
                .with_provider_key_material(Some(p256dh)),
        )
        .await
        .expect("device");
    store
        .publish_notification_from_user_server(
            node.node(),
            &web_push_notification_item("ptl-item-1", "alice@example.com", "dm", 1),
            &owner,
        )
        .await
        .expect("publish");
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("drain");

    assert_eq!(sender.call_count(), 1);
    // Job status must be FAILED, not PUBLISHED — see
    // `all_attempts_with_encoder_bug_signature`.
    let mut rows = query(
        &store,
        "SELECT status FROM push_publish_jobs WHERE item_id = ?",
        db_params!["ptl-item-1"],
    )
    .await;
    let row = rows.next().await.expect("row stream").expect("row present");
    let status: String = row.get(0).expect("status col");
    assert_eq!(
        status, PUBLISH_JOB_STATUS_FAILED,
        "all-PayloadTooLarge fan-out must mark the job FAILED, got {status}"
    );
    // Device row stays active — re-publish succeeds once the
    // encoder/config bug is fixed.
    assert_eq!(
        device_status(&store, node.node(), "web-1").await,
        DEVICE_STATUS_ACTIVE,
        "encoder-bug failure must not disable the device"
    );
}

#[tokio::test]
async fn web_push_capability_is_degraded_without_provider() {
    let store = store().await;
    assert!(matches!(
        store.web_push_capability(),
        waddle_xmpp::push::WebPushCapability::Degraded {
            reason: waddle_xmpp::push::SuppressionReason::Xep0357PushServiceDegraded,
        }
    ));
}

#[tokio::test]
async fn web_push_capability_is_ready_when_provider_wired() {
    let (store, _sender) =
        store_with_web_push_provider(WebPushOutcome::Delivered { status: 201 }).await;
    assert_eq!(
        store.web_push_capability(),
        waddle_xmpp::push::WebPushCapability::Ready
    );
}
