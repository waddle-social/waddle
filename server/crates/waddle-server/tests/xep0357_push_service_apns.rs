//! XEP-0357 Push Service → APNs delivery tests (#529).
//!
//! The Push Service (`push.<domain>`) is the only holder of APNs device
//! tokens. These tests drive a node → Apple-device fan-out through the
//! store's public API with a fake `ApnsSender` that records every
//! request, so they cover:
//!
//! - node-to-APNs-device fan-out with per-device environment routing;
//! - an APNs rejection disabling only the rejected `push_devices` row,
//!   while the XEP-0357 registration and sibling devices stay enabled
//!   (§6), plus the unchanged last-device §6 path;
//! - the minimal-payload privacy default: no sender and no body reach
//!   Apple even when the XEP-0357 summary carried them;
//! - topic mismatch, provider-token refresh, and transient retry.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jid::BareJid;
use minidom::Element;
use p256::pkcs8::EncodePrivateKey;
use waddle_server::db::{Database, IntoParams, Rows};
use waddle_server::db_params;
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_xmpp::inbox::storage::InboxStorage as _;
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::push::apns::{
    ApnsClock, ApnsEnvironment, ApnsKeyId, ApnsOutcome, ApnsPriority, ApnsProviderJwt,
    ApnsProviderTokenSource, ApnsReason, ApnsRequest, ApnsSender, ApnsSignError, ApnsTeamId,
    ApnsTopic, ApnsTransient, CachingApnsTokenSigner, SystemApnsClock,
    APNS_PROVIDER_TOKEN_MIN_REFRESH,
};
use waddle_xmpp::push::types::TransientFailure;
use waddle_xmpp::xep::xep0004::NS_DATA_FORMS;
use waddle_xmpp::xep::xep0357::NS_PUSH;

const BUNDLE_ID: &str = "p4x.waddle.social";
const PUSH_SERVICE_JID: &str = "push.example.com";
const TOKEN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TOKEN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TOKEN_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
/// Mirrors `DEVICE_STATUS_*` in `waddle_server::push_service`.
const DEVICE_STATUS_ACTIVE: &str = "active";
const DEVICE_STATUS_DISABLED: &str = "disabled";
/// Mirrors `apns_dispatch::ATTEMPT_STATUS_APNS_*`.
const ATTEMPT_STATUS_APNS_DELIVERED: &str = "apns-delivered";
const ATTEMPT_STATUS_APNS_GONE: &str = "apns-gone";
const ATTEMPT_STATUS_APNS_TOPIC_MISMATCH: &str = "apns-topic-mismatch";
const ATTEMPT_STATUS_APNS_PROVIDER_AUTH: &str = "apns-provider-auth";
const ATTEMPT_STATUS_APNS_TRANSIENT: &str = "apns-transient";

/// One request as the fake APNs endpoint saw it.
#[derive(Debug, Clone)]
struct CapturedRequest {
    environment: ApnsEnvironment,
    device_token: String,
    topic: String,
    provider_token: ApnsProviderJwt,
    payload: serde_json::Value,
    raw_payload: String,
    priority: ApnsPriority,
    collapse_id: Option<String>,
}

/// Fake `ApnsSender`: per-token outcome sequences (the last one
/// repeats; unknown tokens get `Sent`) and a log of every request.
#[derive(Clone, Default)]
struct FakeApnsSender {
    outcomes: Arc<Vec<(&'static str, Vec<ApnsOutcome>)>>,
    calls: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl FakeApnsSender {
    fn with_outcomes(outcomes: Vec<(&'static str, Vec<ApnsOutcome>)>) -> Self {
        Self {
            outcomes: Arc::new(outcomes),
            calls: Arc::default(),
        }
    }

    fn calls(&self) -> Vec<CapturedRequest> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn calls_for(&self, token: &str) -> Vec<CapturedRequest> {
        self.calls()
            .into_iter()
            .filter(|call| call.device_token == token)
            .collect()
    }
}

impl ApnsSender for FakeApnsSender {
    fn send(
        &self,
        request: ApnsRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = ApnsOutcome> + Send + '_>> {
        let raw_payload =
            String::from_utf8(request.payload.as_slice().to_vec()).expect("UTF-8 JSON payload");
        let captured = CapturedRequest {
            environment: request.environment,
            device_token: request.device_token.as_str().to_string(),
            topic: request.topic.as_str().to_string(),
            provider_token: request.provider_token.clone(),
            payload: serde_json::from_str(&raw_payload).expect("JSON payload"),
            raw_payload,
            priority: request.priority,
            collapse_id: request.collapse_id.map(|id| id.as_str().to_string()),
        };
        let mut calls = self.calls.lock().expect("calls lock");
        let prior = calls
            .iter()
            .filter(|call| call.device_token == captured.device_token)
            .count();
        let outcome = self
            .outcomes
            .iter()
            .find(|(token, _)| *token == captured.device_token)
            .map(|(_, sequence)| sequence[prior.min(sequence.len() - 1)].clone())
            .unwrap_or(ApnsOutcome::Sent { apns_id: None });
        calls.push(captured);
        drop(calls);
        Box::pin(async move { outcome })
    }
}

/// Wall clock the test can move forward, so a cached provider token
/// can be aged past Apple's minimum refresh interval.
struct TestClock(AtomicU64);

impl TestClock {
    fn advance(&self, by: Duration) {
        self.0.fetch_add(by.as_secs(), Ordering::SeqCst);
    }
}

impl ApnsClock for TestClock {
    fn now_unix_seconds(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Real ES256 signer wrapped to count invalidations.
struct CountingTokens {
    inner: CachingApnsTokenSigner,
    clock: Arc<TestClock>,
    invalidations: AtomicUsize,
}

impl CountingTokens {
    /// Mints the cached token now and ages it past the refresh floor, as
    /// a long-running server's token would be when Apple expires it.
    fn age_cached_token(&self) {
        self.inner.current().expect("prime token");
        self.clock.advance(APNS_PROVIDER_TOKEN_MIN_REFRESH);
    }
}

impl ApnsProviderTokenSource for CountingTokens {
    fn current(&self) -> Result<ApnsProviderJwt, ApnsSignError> {
        self.inner.current()
    }

    fn invalidate(&self, rejected: &ApnsProviderJwt) -> bool {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
        self.inner.invalidate(rejected)
    }
}

fn counting_tokens() -> Arc<CountingTokens> {
    let pem = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng)
        .to_pkcs8_pem(Default::default())
        .expect("p8 key");
    let clock = Arc::new(TestClock(AtomicU64::new(
        SystemApnsClock.now_unix_seconds(),
    )));
    Arc::new(CountingTokens {
        inner: CachingApnsTokenSigner::from_pkcs8_pem(
            ApnsTeamId::parse("TEAM123456").expect("team id"),
            ApnsKeyId::parse("KEY1234567").expect("key id"),
            &pem,
            Arc::clone(&clock) as Arc<dyn ApnsClock>,
        )
        .expect("signer"),
        clock,
        invalidations: AtomicUsize::new(0),
    })
}

async fn store_with_apns(
    sender: FakeApnsSender,
    tokens: Arc<CountingTokens>,
) -> DatabasePushServiceStore {
    let store = DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("push-service-apns")
            .await
            .expect("push service db"),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push service store");
    let inbox_storage = Arc::new(
        waddle_server::inbox::DatabaseInboxStorage::from_database(store.database())
            .await
            .expect("inbox storage"),
    );
    let store = store.with_inbox_storage(inbox_storage).with_apns_provider(
        tokens,
        Arc::new(sender),
        ApnsTopic::parse(BUNDLE_ID).expect("topic"),
    );
    // The user server's XEP-0357 registration table lives in the same
    // database; creating its store creates the table.
    waddle_server::push_registrations::DatabasePushRegistrationStore::new(store.database())
        .await
        .expect("registration store");
    store
}

async fn seed_unread_entries(
    store: &DatabasePushServiceStore,
    owner: &BareJid,
    conversation: &str,
    count: u32,
) {
    let inbox = waddle_server::inbox::DatabaseInboxStorage::from_database(store.database())
        .await
        .expect("inbox storage");
    let partner: BareJid = conversation.parse().expect("conversation JID");
    for sequence in 0..count {
        inbox
            .upsert(
                owner,
                InboxEntry::new(
                    partner.clone(),
                    ConversationKind::Direct,
                    format!("archive-{conversation}-{sequence}"),
                    i64::from(sequence),
                ),
                true,
            )
            .await
            .expect("upsert unread entry");
    }
}

fn owner() -> BareJid {
    "alice@example.com".parse().expect("owner jid")
}

async fn register_apple_device(
    store: &DatabasePushServiceStore,
    owner: &BareJid,
    node: &str,
    device_id: &str,
    environment: &str,
    token: &str,
) {
    store
        .upsert_device(
            owner,
            PushDeviceRegistration::new(device_id, node, PushDevicePlatform::Apns, environment)
                .with_provider_token(Some(token.to_string())),
        )
        .await
        .expect("apple device");
}

fn data_form_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(value)
                .build(),
        )
        .build()
}

/// XEP-0357 §4 notification with the summary form (optionally carrying
/// `last-message-sender` / `last-message-body`, which XEP-0357 allows)
/// and the Waddle routing context.
fn notification_item(item_id: &str, message_count: u32, with_sender_and_body: bool) -> PubSubItem {
    let mut summary = Element::builder("x", NS_DATA_FORMS)
        .append(data_form_field("FORM_TYPE", "urn:xmpp:push:summary"))
        .append(data_form_field("message-count", &message_count.to_string()));
    if with_sender_and_body {
        summary = summary
            .append(data_form_field(
                "last-message-sender",
                "mallory@example.com/phone",
            ))
            .append(data_form_field(
                "last-message-body",
                "the launch code is 0000",
            ));
    }
    let context = Element::builder("context", "urn:waddle:push:context:0")
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            "bob@example.com",
        )
        .attr(minidom::rxml::xml_ncname!("class").to_owned(), "dm")
        .attr(
            minidom::rxml::xml_ncname!("stanza-id").to_owned(),
            "stanza-42",
        )
        .build();
    let notification = Element::builder("notification", NS_PUSH)
        .append(summary.build())
        .append(context)
        .build();
    PubSubItem::new(Some(item_id.to_string()), Some(notification))
}

async fn query(store: &DatabasePushServiceStore, sql: &str, params: impl IntoParams) -> Rows {
    let db = store.database();
    let conn = db.guard().await.expect("db guard");
    conn.query(sql, params).await.expect("query")
}

async fn scalar_string(
    store: &DatabasePushServiceStore,
    sql: &str,
    params: impl IntoParams,
) -> String {
    let mut rows = query(store, sql, params).await;
    let row = rows.next().await.expect("row").expect("row present");
    row.get::<String>(0).expect("string column")
}

async fn device_status(store: &DatabasePushServiceStore, node: &str, device_id: &str) -> String {
    scalar_string(
        store,
        "SELECT status FROM push_devices WHERE node = ? AND device_id = ?",
        db_params![node, device_id],
    )
    .await
}

async fn job_status(store: &DatabasePushServiceStore, item_id: &str) -> String {
    scalar_string(
        store,
        "SELECT status FROM push_publish_jobs WHERE item_id = ?",
        db_params![item_id],
    )
    .await
}

async fn attempt_statuses(store: &DatabasePushServiceStore, node: &str) -> Vec<(String, String)> {
    let mut statuses = store
        .delivery_attempts_for_node(node)
        .await
        .expect("attempts")
        .iter()
        .map(|attempt| {
            (
                attempt.device_id().to_string(),
                attempt.status().to_string(),
            )
        })
        .collect::<Vec<_>>();
    statuses.sort();
    statuses
}

async fn enabled_registrations(store: &DatabasePushServiceStore, owner: &BareJid) -> usize {
    use waddle_xmpp::push::PushSubscriptionStore as _;
    waddle_server::push_registrations::DatabasePushRegistrationStore::new(store.database())
        .await
        .expect("registration store")
        .get_for_user(owner.to_string().as_str())
        .await
        .expect("registrations")
        .len()
}

async fn publish_registered(
    store: &DatabasePushServiceStore,
    owner: &BareJid,
    node: &str,
    item: PubSubItem,
) {
    store
        .publish_registered_notification_from_user_server_with_publish_options(
            PUSH_SERVICE_JID,
            node,
            &item,
            owner,
            None,
        )
        .await
        .expect("publish");
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("drain");
}

async fn publish(store: &DatabasePushServiceStore, owner: &BareJid, node: &str, item: PubSubItem) {
    store
        .publish_notification_from_user_server(node, &item, owner)
        .await
        .expect("publish");
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("drain");
}

#[tokio::test]
async fn xep0357_node_fans_out_to_every_apple_device_on_its_own_environment() {
    let sender = FakeApnsSender::default();
    let store = store_with_apns(sender.clone(), counting_tokens()).await;
    let owner = owner();
    let node = store.ensure_node(&owner, BUNDLE_ID).await.expect("node");
    register_apple_device(&store, &owner, node.node(), "iphone", "prod", TOKEN_A).await;
    register_apple_device(&store, &owner, node.node(), "ipad", "prod", TOKEN_B).await;
    register_apple_device(&store, &owner, node.node(), "dev-mac", "sandbox", TOKEN_C).await;
    seed_unread_entries(&store, &owner, "bob@example.com", 4).await;

    publish(
        &store,
        &owner,
        node.node(),
        notification_item("fanout-1", 4, false),
    )
    .await;

    let calls = sender.calls();
    assert_eq!(calls.len(), 3, "one APNs request per active Apple device");
    for (token, environment) in [
        (TOKEN_A, ApnsEnvironment::Production),
        (TOKEN_B, ApnsEnvironment::Production),
        (TOKEN_C, ApnsEnvironment::Sandbox),
    ] {
        let call = sender.calls_for(token);
        assert_eq!(call.len(), 1, "exactly one request for {token}");
        let call = &call[0];
        assert_eq!(call.environment, environment, "per-device APNs host");
        assert_eq!(call.topic, BUNDLE_ID);
        assert_eq!(
            call.priority,
            ApnsPriority::Immediate,
            "DMs wake the device"
        );
        assert_eq!(call.collapse_id.as_deref(), Some("stanza-42"));
        assert_eq!(call.payload["aps"]["badge"], 4);
        assert_eq!(call.payload["waddle"]["node"], node.node());
        assert_eq!(call.payload["waddle"]["item"], "stanza-42");
    }
    // One provider token is signed and reused across the fan-out.
    assert!(calls
        .iter()
        .all(|call| call.provider_token == calls[0].provider_token));
    assert_eq!(
        attempt_statuses(&store, node.node()).await,
        vec![
            (
                "dev-mac".to_string(),
                ATTEMPT_STATUS_APNS_DELIVERED.to_string()
            ),
            (
                "ipad".to_string(),
                ATTEMPT_STATUS_APNS_DELIVERED.to_string()
            ),
            (
                "iphone".to_string(),
                ATTEMPT_STATUS_APNS_DELIVERED.to_string()
            ),
        ]
    );
    assert_eq!(job_status(&store, "fanout-1").await, "published");
}

#[tokio::test]
async fn xep0357_apns_rejection_disables_only_that_device_not_the_registration() {
    let sender = FakeApnsSender::with_outcomes(vec![(
        TOKEN_A,
        vec![ApnsOutcome::DeviceGone {
            status: 410,
            reason: ApnsReason::Unregistered,
        }],
    )]);
    let store = store_with_apns(sender.clone(), counting_tokens()).await;
    let owner = owner();
    let node = store.ensure_node(&owner, BUNDLE_ID).await.expect("node");
    register_apple_device(&store, &owner, node.node(), "gone-phone", "prod", TOKEN_A).await;
    register_apple_device(&store, &owner, node.node(), "live-phone", "prod", TOKEN_B).await;
    store
        .register_first_party_node_for_owner(&owner, PUSH_SERVICE_JID, node.node(), None)
        .await
        .expect("XEP-0357 registration");
    assert_eq!(enabled_registrations(&store, &owner).await, 1);

    publish_registered(
        &store,
        &owner,
        node.node(),
        notification_item("reject-1", 1, false),
    )
    .await;

    assert_eq!(
        device_status(&store, node.node(), "gone-phone").await,
        DEVICE_STATUS_DISABLED,
        "APNs 410 Unregistered disables the rejected device row"
    );
    assert_eq!(
        device_status(&store, node.node(), "live-phone").await,
        DEVICE_STATUS_ACTIVE,
        "the sibling device keeps receiving pushes"
    );
    assert_eq!(
        enabled_registrations(&store, &owner).await,
        1,
        "XEP-0357 §6: the registration stays enabled while a device remains"
    );
    assert_eq!(
        attempt_statuses(&store, node.node()).await,
        vec![
            (
                "gone-phone".to_string(),
                ATTEMPT_STATUS_APNS_GONE.to_string()
            ),
            (
                "live-phone".to_string(),
                ATTEMPT_STATUS_APNS_DELIVERED.to_string()
            ),
        ]
    );

    // The disabled device is no longer part of the fan-out.
    publish_registered(
        &store,
        &owner,
        node.node(),
        notification_item("reject-2", 2, false),
    )
    .await;
    assert_eq!(sender.calls_for(TOKEN_A).len(), 1);
    assert_eq!(sender.calls_for(TOKEN_B).len(), 2);
}

#[tokio::test]
async fn xep0357_bad_device_token_on_the_last_apple_device_disables_the_registration() {
    let sender = FakeApnsSender::with_outcomes(vec![(
        TOKEN_A,
        vec![ApnsOutcome::DeviceGone {
            status: 400,
            reason: ApnsReason::BadDeviceToken,
        }],
    )]);
    let store = store_with_apns(sender, counting_tokens()).await;
    let owner = owner();
    let node = store.ensure_node(&owner, BUNDLE_ID).await.expect("node");
    register_apple_device(&store, &owner, node.node(), "only-phone", "prod", TOKEN_A).await;
    store
        .register_first_party_node_for_owner(&owner, PUSH_SERVICE_JID, node.node(), None)
        .await
        .expect("XEP-0357 registration");

    publish_registered(
        &store,
        &owner,
        node.node(),
        notification_item("last-1", 1, false),
    )
    .await;

    assert_eq!(
        device_status(&store, node.node(), "only-phone").await,
        DEVICE_STATUS_DISABLED
    );
    assert_eq!(
        enabled_registrations(&store, &owner).await,
        0,
        "XEP-0357 §6: no deliverable device remains, so the registration is disabled"
    );
}

#[tokio::test]
async fn xep0357_apns_payload_is_minimal_even_when_the_summary_has_sender_and_body() {
    let sender = FakeApnsSender::default();
    let store = store_with_apns(sender.clone(), counting_tokens()).await;
    let owner = owner();
    let node = store.ensure_node(&owner, BUNDLE_ID).await.expect("node");
    register_apple_device(&store, &owner, node.node(), "iphone", "prod", TOKEN_A).await;
    seed_unread_entries(&store, &owner, "bob@example.com", 7).await;

    publish(
        &store,
        &owner,
        node.node(),
        notification_item("private-1", 7, true),
    )
    .await;

    let calls = sender.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    for leaked in ["mallory", "launch code", "last-message", "body", "sender"] {
        assert!(
            !call.raw_payload.contains(leaked),
            "APNs payload leaked {leaked:?}: {}",
            call.raw_payload
        );
    }
    assert_eq!(
        call.payload,
        serde_json::json!({
            "aps": {
                "alert": { "loc-key": "WADDLE_PUSH_DM" },
                "badge": 7,
                "sound": "default",
                "thread-id": "bob@example.com",
            },
            "waddle": {
                "v": 1,
                "class": "dm",
                "conversation": "bob@example.com",
                "item": "stanza-42",
                "node": node.node(),
            },
        }),
        "only the generic alert, badge and routing context reach Apple"
    );
}

#[tokio::test]
async fn apns_topic_mismatch_is_rejected_before_any_request() {
    let sender = FakeApnsSender::default();
    let store = store_with_apns(sender.clone(), counting_tokens()).await;
    let owner = owner();
    let node = store
        .ensure_node(&owner, "com.example.other-app")
        .await
        .expect("node");
    register_apple_device(&store, &owner, node.node(), "iphone", "prod", TOKEN_A).await;

    publish(
        &store,
        &owner,
        node.node(),
        notification_item("topic-1", 1, false),
    )
    .await;

    assert!(
        sender.calls().is_empty(),
        "no APNs request for a foreign topic"
    );
    assert_eq!(
        attempt_statuses(&store, node.node()).await,
        vec![(
            "iphone".to_string(),
            ATTEMPT_STATUS_APNS_TOPIC_MISMATCH.to_string()
        )]
    );
    assert_eq!(
        device_status(&store, node.node(), "iphone").await,
        DEVICE_STATUS_ACTIVE
    );
    assert_eq!(job_status(&store, "topic-1").await, "failed");
}

#[tokio::test]
async fn apns_expired_provider_token_is_refreshed_and_retried_once() {
    let sender = FakeApnsSender::with_outcomes(vec![(
        TOKEN_A,
        vec![
            ApnsOutcome::ProviderAuth {
                reason: ApnsReason::ExpiredProviderToken,
            },
            ApnsOutcome::Sent { apns_id: None },
        ],
    )]);
    let tokens = counting_tokens();
    tokens.age_cached_token();
    let store = store_with_apns(sender.clone(), Arc::clone(&tokens)).await;
    let owner = owner();
    let node = store.ensure_node(&owner, BUNDLE_ID).await.expect("node");
    register_apple_device(&store, &owner, node.node(), "iphone", "prod", TOKEN_A).await;

    publish(
        &store,
        &owner,
        node.node(),
        notification_item("refresh-1", 1, false),
    )
    .await;

    assert_eq!(
        sender.calls_for(TOKEN_A).len(),
        2,
        "one retry with a fresh token"
    );
    assert_eq!(tokens.invalidations.load(Ordering::SeqCst), 1);
    assert_eq!(
        attempt_statuses(&store, node.node()).await,
        vec![(
            "iphone".to_string(),
            ATTEMPT_STATUS_APNS_DELIVERED.to_string()
        )]
    );
}

#[tokio::test]
async fn apns_provider_token_rejected_twice_keeps_the_device_and_fails_the_job() {
    let sender = FakeApnsSender::with_outcomes(vec![(
        TOKEN_A,
        vec![ApnsOutcome::ProviderAuth {
            reason: ApnsReason::InvalidProviderToken,
        }],
    )]);
    let tokens = counting_tokens();
    tokens.age_cached_token();
    let store = store_with_apns(sender.clone(), tokens).await;
    let owner = owner();
    let node = store.ensure_node(&owner, BUNDLE_ID).await.expect("node");
    register_apple_device(&store, &owner, node.node(), "iphone", "prod", TOKEN_A).await;

    publish(
        &store,
        &owner,
        node.node(),
        notification_item("auth-1", 1, false),
    )
    .await;

    assert_eq!(sender.calls_for(TOKEN_A).len(), 2, "retried exactly once");
    assert_eq!(
        attempt_statuses(&store, node.node()).await,
        vec![(
            "iphone".to_string(),
            ATTEMPT_STATUS_APNS_PROVIDER_AUTH.to_string()
        )]
    );
    assert_eq!(
        device_status(&store, node.node(), "iphone").await,
        DEVICE_STATUS_ACTIVE,
        "a provider key problem is not the device's fault"
    );
    assert_eq!(job_status(&store, "auth-1").await, "failed");
}

#[tokio::test]
async fn apns_rejection_of_a_young_provider_token_is_not_retried() {
    let sender = FakeApnsSender::with_outcomes(vec![(
        TOKEN_A,
        vec![ApnsOutcome::ProviderAuth {
            reason: ApnsReason::InvalidProviderToken,
        }],
    )]);
    let tokens = counting_tokens();
    let store = store_with_apns(sender.clone(), Arc::clone(&tokens)).await;
    let owner = owner();
    let node = store.ensure_node(&owner, BUNDLE_ID).await.expect("node");
    register_apple_device(&store, &owner, node.node(), "iphone", "prod", TOKEN_A).await;

    publish(
        &store,
        &owner,
        node.node(),
        notification_item("young-1", 1, false),
    )
    .await;

    // A fresh token would not fix a key problem, and refreshing inside
    // 20 minutes earns TooManyProviderTokenUpdates.
    assert_eq!(sender.calls_for(TOKEN_A).len(), 1, "no retry");
    assert_eq!(
        attempt_statuses(&store, node.node()).await,
        vec![(
            "iphone".to_string(),
            ATTEMPT_STATUS_APNS_PROVIDER_AUTH.to_string()
        )]
    );
    assert_eq!(job_status(&store, "young-1").await, "failed");
}

#[tokio::test]
async fn apns_transient_failure_requeues_without_resending_to_delivered_siblings() {
    let sender = FakeApnsSender::with_outcomes(vec![(
        TOKEN_A,
        vec![
            ApnsOutcome::Transient {
                cause: ApnsTransient::Failure(TransientFailure::ServerError { status: 503 }),
                retry_after: Some(Duration::from_secs(120)),
            },
            ApnsOutcome::Sent { apns_id: None },
        ],
    )]);
    let store = store_with_apns(sender.clone(), counting_tokens()).await;
    let owner = owner();
    let node = store.ensure_node(&owner, BUNDLE_ID).await.expect("node");
    register_apple_device(&store, &owner, node.node(), "flaky-phone", "prod", TOKEN_A).await;
    register_apple_device(&store, &owner, node.node(), "good-phone", "prod", TOKEN_B).await;
    seed_unread_entries(&store, &owner, "bob@example.com", 1).await;

    publish(
        &store,
        &owner,
        node.node(),
        notification_item("retry-1", 1, false),
    )
    .await;

    assert_eq!(job_status(&store, "retry-1").await, "queued");
    assert_eq!(sender.calls_for(TOKEN_A)[0].payload["aps"]["badge"], 1);
    assert_eq!(sender.calls_for(TOKEN_B)[0].payload["aps"]["badge"], 1);
    assert_eq!(
        device_status(&store, node.node(), "flaky-phone").await,
        DEVICE_STATUS_ACTIVE
    );
    let statuses = attempt_statuses(&store, node.node()).await;
    assert!(statuses.contains(&(
        "flaky-phone".to_string(),
        ATTEMPT_STATUS_APNS_TRANSIENT.to_string()
    )));

    // The queued job retains its XEP-0357 payload, but the APNs app-icon
    // count is refreshed from the shared inbox at each provider dispatch.
    seed_unread_entries(&store, &owner, "carol@example.com", 1).await;

    {
        let db = store.database();
        let conn = db.guard().await.expect("db guard");
        conn.execute("UPDATE push_publish_jobs SET next_retry_at_ms = 0", ())
            .await
            .expect("make the job retry-eligible");
    }
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("retry drain");

    assert_eq!(job_status(&store, "retry-1").await, "published");
    let flaky_calls = sender.calls_for(TOKEN_A);
    assert_eq!(flaky_calls.len(), 2);
    assert_eq!(flaky_calls[1].payload["aps"]["badge"], 2);
    assert_eq!(
        sender.calls_for(TOKEN_B).len(),
        1,
        "#1123: an already-delivered Apple device is not pushed twice"
    );
}
