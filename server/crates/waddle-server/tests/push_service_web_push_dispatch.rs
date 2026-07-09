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

/// A `WebPushSender` that picks its outcome by matching the request
/// endpoint against a configured substring, and records every
/// endpoint it was invoked for so tests can assert exactly which
/// devices were (re-)dispatched. Each endpoint carries a SEQUENCE of
/// outcomes consumed per call (the last one repeats), so a test can
/// model "transient on the first pass, permanent on the retry".
#[derive(Clone)]
struct PerEndpointSender {
    outcomes: Arc<Vec<(String, Vec<WebPushOutcome>)>>,
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}

impl PerEndpointSender {
    fn new(outcomes: Vec<(String, WebPushOutcome)>) -> Self {
        Self::with_sequences(
            outcomes
                .into_iter()
                .map(|(fragment, outcome)| (fragment, vec![outcome]))
                .collect(),
        )
    }

    fn with_sequences(outcomes: Vec<(String, Vec<WebPushOutcome>)>) -> Self {
        Self {
            outcomes: Arc::new(outcomes),
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn calls_matching(&self, endpoint_fragment: &str) -> usize {
        self.calls
            .lock()
            .expect("calls lock")
            .iter()
            .filter(|endpoint| endpoint.contains(endpoint_fragment))
            .count()
    }
}

impl WebPushSender for PerEndpointSender {
    fn send(
        &self,
        request: WebPushRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = WebPushOutcome> + Send + '_>> {
        let endpoint = request.endpoint.to_string();
        let mut calls = self.calls.lock().expect("calls lock");
        let prior_calls = calls
            .iter()
            .filter(|called| called.as_str() == endpoint)
            .count();
        calls.push(endpoint.clone());
        drop(calls);
        let outcome = self
            .outcomes
            .iter()
            .find(|(fragment, _)| endpoint.contains(fragment.as_str()))
            .map(|(_, sequence)| sequence[prior_calls.min(sequence.len() - 1)].clone())
            .unwrap_or(WebPushOutcome::Delivered { status: 201 });
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
    let sender = FixedOutcomeSender::new(outcome);
    let store = store_with_web_push_sender(Arc::new(sender.clone())).await;
    (store, sender)
}

async fn store_with_web_push_sender(sender: Arc<dyn WebPushSender>) -> DatabasePushServiceStore {
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
    let sub = VapidSub::default_for_domain("example.com").expect("vapid sub");
    store.with_web_push_provider(signer, sender, sub)
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

/// Register a fresh active web device with valid subscription
/// material whose endpoint contains `endpoint_fragment`.
async fn register_web_device(
    store: &DatabasePushServiceStore,
    owner: &BareJid,
    node: &str,
    device_id: &str,
    endpoint_fragment: &str,
) {
    let (p256dh, auth) = fresh_subscription_material();
    store
        .upsert_device(
            owner,
            PushDeviceRegistration::new(device_id, node, PushDevicePlatform::Web, "test")
                .with_provider_endpoint(Some(format!(
                    "https://push.example.com/{endpoint_fragment}"
                )))
                .with_provider_token(Some(auth))
                .with_provider_key_material(Some(p256dh)),
        )
        .await
        .expect("device");
}

/// Make every queued publish job immediately retry-eligible.
async fn force_retry_eligibility(store: &DatabasePushServiceStore) {
    let db = store.database();
    let conn = db.guard().await.expect("db guard");
    conn.execute("UPDATE push_publish_jobs SET next_retry_at_ms = 0", ())
        .await
        .expect("reset retry deadline");
}

// #1123: a retried publish job must not re-push to devices whose
// previous attempt for the same item already succeeded — one
// rate-limited sibling must not turn into duplicate OS notifications
// on every other device sharing the node.
#[tokio::test]
async fn retried_job_skips_devices_already_delivered() {
    let sender = PerEndpointSender::new(vec![
        (
            "delivered-device".to_string(),
            WebPushOutcome::Delivered { status: 201 },
        ),
        (
            "flaky-device".to_string(),
            WebPushOutcome::Transient {
                kind: waddle_xmpp::push::types::TransientFailure::Network,
            },
        ),
    ]);
    let store = store_with_web_push_sender(Arc::new(sender.clone())).await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    register_web_device(&store, &owner, node.node(), "web-ok", "delivered-device").await;
    register_web_device(&store, &owner, node.node(), "web-flaky", "flaky-device").await;
    store
        .publish_notification_from_user_server(
            node.node(),
            &web_push_notification_item("partial-item-1", "alice@example.com", "dm", 1),
            &owner,
        )
        .await
        .expect("publish");

    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("first drain");
    assert_eq!(sender.calls_matching("delivered-device"), 1);
    assert_eq!(sender.calls_matching("flaky-device"), 1);

    force_retry_eligibility(&store).await;
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("retry drain");

    assert_eq!(
        sender.calls_matching("delivered-device"),
        1,
        "#1123: the already-delivered device must not receive a duplicate web-push on retry"
    );
    assert_eq!(
        sender.calls_matching("flaky-device"),
        2,
        "the transiently-failing device must still be retried"
    );
}

// #1123 companion: when every remaining device for a requeued job has
// already succeeded (e.g. the failing sibling was disabled between
// retries), the job must finalize as published instead of spinning in
// the no-active-devices retry loop.
#[tokio::test]
async fn retried_job_with_all_devices_delivered_finalizes_published() {
    let sender = PerEndpointSender::new(vec![
        (
            "delivered-device".to_string(),
            WebPushOutcome::Delivered { status: 201 },
        ),
        (
            "gone-device".to_string(),
            WebPushOutcome::Transient {
                kind: waddle_xmpp::push::types::TransientFailure::Network,
            },
        ),
    ]);
    let store = store_with_web_push_sender(Arc::new(sender.clone())).await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    register_web_device(&store, &owner, node.node(), "web-ok", "delivered-device").await;
    register_web_device(&store, &owner, node.node(), "web-gone", "gone-device").await;
    store
        .publish_notification_from_user_server(
            node.node(),
            &web_push_notification_item("all-done-item-1", "alice@example.com", "dm", 1),
            &owner,
        )
        .await
        .expect("publish");
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("first drain");

    // The flaky device disappears (user unregistered / device
    // disabled) before the retry fires.
    let db = store.database();
    let conn = db.guard().await.expect("db guard");
    conn.execute(
        "UPDATE push_devices SET status = 'disabled' WHERE device_id = 'web-gone'",
        (),
    )
    .await
    .expect("disable flaky device");
    drop(conn);
    force_retry_eligibility(&store).await;

    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("retry drain");

    assert_eq!(
        sender.calls_matching("delivered-device"),
        1,
        "#1123: no duplicate web-push when the retry has nothing left to send"
    );
    let mut rows = query(
        &store,
        "SELECT status FROM push_publish_jobs WHERE item_id = 'all-done-item-1'",
        (),
    )
    .await;
    let row = rows.next().await.expect("job row").expect("job present");
    assert_eq!(
        row.get::<String>(0).expect("status"),
        "published",
        "a fully-delivered job must finalize as published, not requeue forever"
    );
}

// #1123 companion (Codex review): on a retry whose fan-out excluded an
// already-delivered sibling, a permanent encoder-bug status from the
// one REMAINING device must not trip the "all devices returned an
// encoder-bug status" FAILED classification — the payload demonstrably
// delivered to the sibling, so the job completes as PUBLISHED (the
// same outcome a single mixed-result pass would produce).
#[tokio::test]
async fn retry_with_prior_delivery_does_not_fail_job_on_uniform_encoder_status() {
    let sender = PerEndpointSender::with_sequences(vec![
        (
            "delivered-device".to_string(),
            vec![WebPushOutcome::Delivered { status: 201 }],
        ),
        (
            "flaky-device".to_string(),
            vec![
                WebPushOutcome::Transient {
                    kind: waddle_xmpp::push::types::TransientFailure::Network,
                },
                WebPushOutcome::PayloadTooLarge { status: 413 },
            ],
        ),
    ]);
    let store = store_with_web_push_sender(Arc::new(sender.clone())).await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    register_web_device(&store, &owner, node.node(), "web-ok", "delivered-device").await;
    register_web_device(&store, &owner, node.node(), "web-flaky", "flaky-device").await;
    store
        .publish_notification_from_user_server(
            node.node(),
            &web_push_notification_item("mixed-retry-item-1", "alice@example.com", "dm", 1),
            &owner,
        )
        .await
        .expect("publish");
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("first drain");
    force_retry_eligibility(&store).await;
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("retry drain");

    assert_eq!(sender.calls_matching("delivered-device"), 1);
    assert_eq!(sender.calls_matching("flaky-device"), 2);
    let mut rows = query(
        &store,
        "SELECT status FROM push_publish_jobs WHERE item_id = 'mixed-retry-item-1'",
        (),
    )
    .await;
    let row = rows.next().await.expect("job row").expect("job present");
    assert_eq!(
        row.get::<String>(0).expect("status"),
        "published",
        "a prior delivered sibling means the retry's uniform 413 is not an all-device encoder bug"
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
