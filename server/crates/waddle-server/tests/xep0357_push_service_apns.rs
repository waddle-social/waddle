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

pub mod ingress_support;

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
use waddle_server::notification_outbox::{DrainBatch, DrainScope};
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_xmpp::inbox::storage::InboxStorage as _;
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::push::apns::{
    ApnsClock, ApnsEnvironment, ApnsKeyId, ApnsOutcome, ApnsPriority, ApnsProviderJwt,
    ApnsProviderTokenSource, ApnsPushType, ApnsReason, ApnsRequest, ApnsSender, ApnsSignError,
    ApnsTeamId, ApnsTopic, ApnsTransient, CachingApnsTokenSigner, SystemApnsClock,
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
const ATTEMPT_STATUS_APNS_SERVER_ERROR: &str = "apns-server-error";

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
    push_type: ApnsPushType,
    expiration: u64,
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
            push_type: request.push_type,
            expiration: request.expiration.unix_seconds(),
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
    let store = store
        .with_inbox_storage(inbox_storage)
        .with_apns_provider(
            tokens,
            Arc::new(sender),
            ApnsTopic::parse(BUNDLE_ID).expect("topic"),
        )
        .expect("valid alert and VoIP topics");
    // The user server's XEP-0357 registration table lives in the same
    // database; creating its store creates the table.
    waddle_server::push_registrations::DatabasePushRegistrationStore::try_new(store.database())
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
    waddle_server::push_registrations::DatabasePushRegistrationStore::try_new(store.database())
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
        assert_eq!(call.push_type, ApnsPushType::Alert);
        assert!(call.expiration > SystemApnsClock.now_unix_seconds());
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
        ATTEMPT_STATUS_APNS_SERVER_ERROR.to_string()
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

#[tokio::test]
async fn connection_setup_failures_stop_at_budget_and_preserve_only_prior_uncertainty() {
    for prior_unknown in [false, true] {
        let sender = FakeApnsSender::with_outcomes(vec![(
            TOKEN_A,
            vec![
                ApnsOutcome::Transient {
                    cause: ApnsTransient::Failure(TransientFailure::Connect),
                    retry_after: None
                };
                25
            ],
        )]);
        let store = store_with_apns(sender.clone(), counting_tokens()).await;
        let owner = owner();
        let node = store.ensure_node(&owner, BUNDLE_ID).await.expect("node");
        register_apple_device(&store, &owner, node.node(), "phone", "prod", TOKEN_A).await;
        publish(
            &store,
            &owner,
            node.node(),
            notification_item("connect-budget", 1, false),
        )
        .await;
        if prior_unknown {
            store
                .database()
                .guard()
                .await
                .expect("guard")
                .execute("UPDATE push_publish_jobs SET uncertain_send = 1", ())
                .await
                .expect("earlier unknown send");
        }
        for completed in 1..=24 {
            let mut rows = query(&store, "SELECT status, uncertain_send, attempt_count FROM push_publish_jobs WHERE item_id = 'connect-budget'", ()).await;
            let row = rows.next().await.expect("row").expect("job");
            assert_eq!(
                row.get::<String>(0).expect("status"),
                if completed < 24 { "queued" } else { "failed" }
            );
            assert_eq!(
                row.get::<i64>(1).expect("uncertainty"),
                i64::from(prior_unknown)
            );
            assert_eq!(row.get::<i64>(2).expect("count"), completed);
            assert_eq!(sender.calls_for(TOKEN_A).len(), completed as usize);
            if completed < 24 {
                store
                    .database()
                    .guard()
                    .await
                    .expect("guard")
                    .execute("UPDATE push_publish_jobs SET next_retry_at_ms = 0", ())
                    .await
                    .expect("retry due");
                store
                    .drain_queued_notification_publish_jobs(16)
                    .await
                    .expect("next attempt");
            }
        }
        assert!(store.queued_publish_jobs().await.expect("queue").is_empty());
        assert!(attempt_statuses(&store, node.node())
            .await
            .iter()
            .all(|(_, status)| status == "apns-connect-failed"));
        store
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("exhausted work idle");
        assert_eq!(sender.calls_for(TOKEN_A).len(), 24);
        assert_eq!(
            device_status(&store, node.node(), "phone").await,
            DEVICE_STATUS_ACTIVE
        );
    }
}

struct OfflinePushEnvironment {
    connections: waddle_xmpp::registry::ConnectionRegistry,
    pending: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage>,
}

impl waddle_server::ingress::RecoveryEnvironment for OfflinePushEnvironment {
    fn recovery_deps(&self) -> waddle_server::ingress::Deps<'_> {
        let mut deps = waddle_server::ingress::Deps::new(&self.connections, "example.com");
        deps.pending_delivery_storage = Some(&self.pending);
        deps
    }
}

/// Freeze the same offline obligations recorded by the delivery interpreter.
/// The canonical persistence boundary independently re-derives these intrinsic
/// fields from the raw message envelope before accepting the candidate.
fn offline_submission(
    fixture: &ingress_support::IngressFixture,
    stanza_id: &str,
    offer: Option<waddle_xmpp::xep::xep0353::CallOffer>,
) -> waddle_server::ingress::IngressSubmission {
    use waddle_server::ingress::effects::delivery::{
        ExternalDeliveryEffect, PreparedOfflineNotification,
    };
    use waddle_server::ingress::effects::{Effect, ExternalEffect};
    use waddle_server::ingress::PlannedEffect;
    use waddle_server::notification_outbox::{
        NotificationCall, NotificationCandidate, NotificationMessageHints,
    };
    use waddle_xmpp::ingress::{
        DigestContext, DigestInput, IngressEffectIntent, NotificationActivityMutation,
        NotificationCandidateOutcome, PendingDeliveryMutation,
    };
    use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};
    use waddle_xmpp::xep::xep0334::{add_hint, Hint};
    use waddle_xmpp::xep::xep0353::build_propose;
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::jingle::SessionId;

    let mut submission = fixture.submission(Some(stanza_id), "private direct-message body");
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    let stamp = StanzaId::new(stanza_id, recipient.clone().into());
    let candidate = if let Some(offer) = offer {
        submission.plan.sanitized_message.bodies.clear();
        submission
            .plan
            .sanitized_message
            .payloads
            .push(build_propose(SessionId("call-session-1".into()), offer));
        add_hint(&mut submission.plan.sanitized_message, Hint::Store);
        NotificationCandidate::call_with_hints(
            recipient.clone(),
            submission.sender.clone().into(),
            stamp.clone(),
            NotificationCall {
                session_id: SessionId("call-session-1".into()),
                video: offer.video,
            },
            NotificationMessageHints::none(),
        )
        .expect("prepared call")
    } else {
        NotificationCandidate::direct_message(
            recipient.clone(),
            submission.sender.clone().into(),
            stamp.clone(),
            false,
        )
        .expect("prepared direct message")
        .with_last_message_body(Some("private direct-message body".to_owned()))
    };
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("canonical message digest");
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Archived(stamp.clone()),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    submission.plan.intents.extend([
        IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Archived {
                recipient: recipient.clone(),
                row_id: row.id.clone(),
                archive_stanza_id: stamp.clone(),
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: recipient.clone(),
                archive_stanza_id: stamp.clone(),
                outcome: NotificationCandidateOutcome::Inserted,
            },
        },
        IngressEffectIntent::NotificationActivityPreview {
            owner: recipient.clone(),
            mutation: NotificationActivityMutation::OfflineDelivery {
                conversation: recipient,
                archive_stanza_id: stamp,
            },
        },
    ]);
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery {
                row,
                prepared_notification: PreparedOfflineNotification::Prepared(Box::new(candidate)),
                original_message: Box::new(submission.plan.sanitized_message.clone()),
            }),
        )));
    submission
}

async fn execute_offline_notification(
    fixture: &ingress_support::IngressFixture,
    authority: &waddle_server::ingress::IngressAuthority,
    environment: &dyn waddle_server::ingress::RecoveryEnvironment,
    submission: &waddle_server::ingress::IngressSubmission,
    expected_terminal_count: i64,
) {
    let decision = authority.commit(submission).await;
    assert_eq!(
        decision.class,
        waddle_server::ingress::IngressDecisionClass::Accepted
    );
    // Execute inside the call's receipt window. Maintenance deliberately waits
    // sixty seconds before rebuilding a lost decision, longer than a live call.
    let _report = authority
        .execute(
            &decision,
            &waddle_server::ingress::ImmediateSink,
            &environment.recovery_deps(),
        )
        .await;
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        expected_terminal_count * 3,
        "pending={}, candidates={} after offline execution",
        fixture.count("pending_delivery").await,
        fixture.count("notification_candidates").await,
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        expected_terminal_count,
        "offline notification execution settles all canonical obligations",
    );
}

#[tokio::test]
async fn offline_propose_reaches_only_pushkit_and_plain_dm_reaches_only_alert_token() {
    use waddle_server::notification_activity::NoopActivityReader;
    use waddle_server::notification_outbox::{
        NoopDndReader, NoopRoomPolicy, NotificationClass, NotificationDrainDeps,
        NotificationOutboxStore,
    };
    use waddle_server::notification_settings_projection::NotificationSettingsProjectionStore;
    use waddle_server::pending_delivery::DatabasePendingDeliveryStorage;
    use waddle_server::push_registrations::DatabasePushRegistrationStore;
    use waddle_xmpp::pending_delivery::QuotaPolicy;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;
    use waddle_xmpp::xep::xep0353::CallOffer;

    let fixture = ingress_support::IngressFixture::sqlite().await;
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    fixture.execute("INSERT INTO users (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)", db_params![recipient.to_string(), "juliet", "juliet", "juliet", chrono::Utc::now().to_rfc3339(), chrono::Utc::now().to_rfc3339()]).await;
    assert_eq!(
        fixture
            .count("sessions WHERE user_jid = 'juliet@example.com'")
            .await,
        0,
        "recipient account has no session"
    );
    let sender = FakeApnsSender::default();
    let inbox = Arc::new(
        waddle_server::inbox::DatabaseInboxStorage::from_database(fixture.db.clone())
            .await
            .expect("inbox"),
    );
    let push = DatabasePushServiceStore::new_with_secret_key(
        fixture.db.clone(),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push store")
    .with_inbox_storage(inbox.clone())
    .with_apns_provider(
        counting_tokens(),
        Arc::new(sender.clone()),
        ApnsTopic::parse(BUNDLE_ID).expect("bundle topic"),
    )
    .expect("valid alert and VoIP topics");
    let subscriptions = DatabasePushRegistrationStore::try_new(fixture.db.clone())
        .await
        .expect("registration store");
    let alert_node = push
        .ensure_node(&recipient, BUNDLE_ID)
        .await
        .expect("alert node");
    let voip_node = push
        .ensure_node(&recipient, "p4x.waddle.social.voip")
        .await
        .expect("VoIP node");
    for (node, token) in [(alert_node.node(), TOKEN_A), (voip_node.node(), TOKEN_B)] {
        register_apple_device(&push, &recipient, node, "iphone", "prod", token).await;
        push.register_first_party_node_for_owner(&recipient, PUSH_SERVICE_JID, node, None)
            .await
            .expect("enable node");
    }
    let outbox = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("outbox");
    let environment: Arc<dyn waddle_server::ingress::RecoveryEnvironment> =
        Arc::new(OfflinePushEnvironment {
            connections: waddle_xmpp::registry::ConnectionRegistry::new(),
            pending: Arc::new(
                DatabasePendingDeliveryStorage::open(
                    Some(fixture.db.database_url()),
                    QuotaPolicy::Unlimited,
                )
                .await
                .expect("pending delivery"),
            ),
        });
    let authority = fixture.authority().await;
    authority.bind_recovery_environment(Arc::downgrade(&environment));
    let service: BareJid = PUSH_SERVICE_JID.parse().expect("push service");
    let blocking = InMemoryBlockingStorage::new();
    let settings = NotificationSettingsProjectionStore::new(fixture.db.clone());
    let offer = offline_submission(
        &fixture,
        "offline-call-archive",
        Some(CallOffer::audio_video()),
    );
    execute_offline_notification(&fixture, &authority, environment.as_ref(), &offer, 1).await;
    let candidates = outbox.pending_candidates(16).await.expect("call candidate");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].class(), NotificationClass::Call);
    assert!(
        inbox.list(&recipient).await.expect("inbox").is_empty(),
        "call never projects into inbox"
    );
    let created_at_ms = fixture.optional_text("SELECT CAST(created_at_ms AS TEXT) FROM notification_candidates WHERE stanza_id = 'offline-call-archive'")
        .await.expect("receipt timestamp").parse::<u64>().expect("timestamp");
    let expires = created_at_ms / 1000 + 45;
    assert_eq!(
        outbox
            .drain_pending_candidates_into_outbox(
                &subscriptions,
                &blocking,
                &settings,
                NotificationDrainDeps::new(&NoopRoomPolicy, &NoopDndReader, &NoopActivityReader),
                &service,
                DrainBatch {
                    scope: DrainScope::All,
                    batch_size: 16
                },
            )
            .await
            .expect("drain call candidate"),
        1
    );
    let jobs = outbox
        .claim_due_outbox_jobs(DrainScope::All, 16)
        .await
        .expect("claim call jobs");
    assert_eq!(jobs.len(), 2, "one job for each registered node");
    for job in jobs {
        outbox
            .publish_claimed_job(
                &job,
                &push,
                &subscriptions,
                inbox.as_ref(),
                &blocking,
                &service,
            )
            .await
            .expect("publish call with zero unread");
    }
    push.drain_queued_notification_publish_jobs(16)
        .await
        .expect("provider drain");
    assert!(
        sender.calls_for(TOKEN_A).is_empty(),
        "alert token receives no call"
    );
    let calls = sender.calls_for(TOKEN_B);
    assert_eq!(calls.len(), 1, "one request reaches PushKit token");
    let call = &calls[0];
    assert_eq!(call.push_type, ApnsPushType::Voip);
    assert_eq!(call.topic, "p4x.waddle.social.voip");
    assert_eq!(call.priority, ApnsPriority::Immediate);
    assert_eq!(call.expiration, expires);
    assert!(call.collapse_id.is_none());
    assert_eq!(
        call.payload,
        serde_json::json!({
            "waddle": {
                "v": 1, "class": "call", "conversation": "romeo@example.com",
                "item": "offline-call-archive", "node": voip_node.node(),
                "call": { "sid": "call-session-1", "video": true, "expires": expires }
            }
        })
    );
    for leaked in ["aps", "body", "sender", "display", "private direct-message"] {
        assert!(
            !call.raw_payload.contains(leaked),
            "call payload leaked {leaked}"
        );
    }
    let dm = offline_submission(&fixture, "offline-dm-archive", None);
    execute_offline_notification(&fixture, &authority, environment.as_ref(), &dm, 2).await;
    seed_unread_entries(&push, &recipient, "romeo@example.com", 1).await;
    assert_eq!(
        outbox
            .drain_pending_candidates_into_outbox(
                &subscriptions,
                &blocking,
                &settings,
                NotificationDrainDeps::new(&NoopRoomPolicy, &NoopDndReader, &NoopActivityReader),
                &service,
                DrainBatch {
                    scope: DrainScope::All,
                    batch_size: 16
                },
            )
            .await
            .expect("drain DM candidate"),
        1
    );
    for job in outbox
        .claim_due_outbox_jobs(DrainScope::All, 16)
        .await
        .expect("claim DM jobs")
    {
        outbox
            .publish_claimed_job(
                &job,
                &push,
                &subscriptions,
                inbox.as_ref(),
                &blocking,
                &service,
            )
            .await
            .expect("publish DM");
    }
    push.drain_queued_notification_publish_jobs(16)
        .await
        .expect("DM provider drain");
    let alerts = sender.calls_for(TOKEN_A);
    assert_eq!(alerts.len(), 1, "plain DM reaches alert token");
    assert_eq!(alerts[0].push_type, ApnsPushType::Alert);
    assert_eq!(
        alerts[0].payload["aps"]["alert"]["loc-key"],
        "WADDLE_PUSH_DM"
    );
    assert_eq!(alerts[0].payload["waddle"]["item"], "offline-dm-archive");
    assert_eq!(
        sender.calls_for(TOKEN_B).len(),
        1,
        "PushKit token receives no plain DM"
    );
    assert_eq!(fixture.count("pending_delivery").await, 2);
    assert!(authority.drain_and_join(Duration::from_secs(15)).await);
    drop(authority);
    drop(environment);
    drop(outbox);
    drop(subscriptions);
    drop(push);
    drop(inbox);
    fixture.close().await;
}

fn item_with_call_context(item_id: &str, class: &str, call: Option<Element>) -> PubSubItem {
    use waddle_server::notification_outbox::WADDLE_PUSH_CONTEXT_NS;
    let mut context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            "bob@example.com",
        )
        .attr(minidom::rxml::xml_ncname!("class").to_owned(), class)
        .attr(
            minidom::rxml::xml_ncname!("stanza-id").to_owned(),
            "call-stanza",
        );
    if let Some(call) = call {
        context = context.append(call);
    }
    PubSubItem::new(
        Some(item_id.to_owned()),
        Some(
            Element::builder("notification", NS_PUSH)
                .append(context.build())
                .build(),
        ),
    )
}

fn call_context_child(sid: &str, video: &str, expires: &str) -> Element {
    Element::builder(
        "call",
        waddle_server::notification_outbox::WADDLE_PUSH_CONTEXT_NS,
    )
    .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid)
    .attr(minidom::rxml::xml_ncname!("video").to_owned(), video)
    .attr(minidom::rxml::xml_ncname!("expires").to_owned(), expires)
    .build()
}

#[tokio::test]
async fn missed_call_is_an_alert_on_ordinary_nodes_and_not_applicable_to_fcm() {
    let sender = FakeApnsSender::default();
    let store = store_with_apns(sender.clone(), counting_tokens()).await;
    let owner = owner();
    let alert = store
        .ensure_node(&owner, BUNDLE_ID)
        .await
        .expect("alert node");
    let voip = store
        .ensure_node(&owner, "p4x.waddle.social.voip")
        .await
        .expect("VoIP node");
    register_apple_device(&store, &owner, alert.node(), "iphone", "prod", TOKEN_A).await;
    register_apple_device(&store, &owner, voip.node(), "pushkit", "prod", TOKEN_B).await;
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("android", alert.node(), PushDevicePlatform::Fcm, "prod"),
        )
        .await
        .expect("FCM device");
    seed_unread_entries(&store, &owner, "bob@example.com", 3).await;
    seed_unread_entries(&store, &owner, "carol@example.com", 2).await;
    for (node, item) in [(alert.node(), "missed-alert"), (voip.node(), "missed-voip")] {
        publish(
            &store,
            &owner,
            node,
            item_with_call_context(item, "missed_call", None),
        )
        .await;
    }
    let calls = sender.calls_for(TOKEN_A);
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.push_type, ApnsPushType::Alert);
    assert_eq!(call.topic, BUNDLE_ID);
    assert_eq!(call.priority, ApnsPriority::Immediate);
    assert_eq!(
        call.payload,
        serde_json::json!({
            "aps": {
                "alert": { "loc-key": "WADDLE_PUSH_MISSED_CALL" },
                "badge": 5, "sound": "default", "thread-id": "bob@example.com"
            },
            "waddle": {
                "v": 1, "class": "missed_call", "conversation": "bob@example.com",
                "item": "call-stanza", "node": alert.node()
            }
        })
    );
    assert!(
        sender.calls_for(TOKEN_B).is_empty(),
        "missed call never reaches PushKit"
    );
    assert_eq!(
        attempt_statuses(&store, alert.node()).await,
        vec![
            ("android".to_owned(), "not-applicable".to_owned()),
            (
                "iphone".to_owned(),
                ATTEMPT_STATUS_APNS_DELIVERED.to_owned()
            ),
        ]
    );
    assert!(attempt_statuses(&store, voip.node()).await.is_empty());
    for item in ["missed-alert", "missed-voip"] {
        assert_eq!(job_status(&store, item).await, "published");
    }
    assert_eq!(
        device_status(&store, alert.node(), "android").await,
        DEVICE_STATUS_ACTIVE
    );
}

#[tokio::test]
async fn malformed_call_metadata_fails_without_sending_or_disabling_devices() {
    let sender = FakeApnsSender::default();
    let store = store_with_apns(sender.clone(), counting_tokens()).await;
    let owner = owner();
    let node = store
        .ensure_node(&owner, "p4x.waddle.social.voip")
        .await
        .expect("VoIP node");
    register_apple_device(&store, &owner, node.node(), "iphone", "prod", TOKEN_A).await;
    let expires = (SystemApnsClock.now_unix_seconds() + 45).to_string();
    for (index, child) in [
        None,
        Some(call_context_child("", "false", &expires)),
        Some(call_context_child("sid", "yes", &expires)),
        Some(call_context_child("sid", "false", "invalid")),
        Some(Element::builder("call", NS_PUSH).build()),
        Some(
            Element::builder(
                "call",
                waddle_server::notification_outbox::WADDLE_PUSH_CONTEXT_NS,
            )
            .build(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let item_id = format!("invalid-call-{index}");
        publish(
            &store,
            &owner,
            node.node(),
            item_with_call_context(&item_id, "call", child),
        )
        .await;
        assert_eq!(job_status(&store, &item_id).await, "failed");
        assert!(scalar_string(
            &store,
            "SELECT last_error FROM push_publish_jobs WHERE item_id = ?",
            db_params![item_id]
        )
        .await
        .contains("payload parse failed"));
    }
    assert!(sender.calls().is_empty());
    assert_eq!(
        device_status(&store, node.node(), "iphone").await,
        DEVICE_STATUS_ACTIVE
    );

    // Unknown call children cannot change the established alert class or payload.
    let alert_node = store
        .ensure_node(&owner, BUNDLE_ID)
        .await
        .expect("alert node");
    register_apple_device(
        &store,
        &owner,
        alert_node.node(),
        "alert-phone",
        "prod",
        TOKEN_B,
    )
    .await;
    publish(
        &store,
        &owner,
        alert_node.node(),
        item_with_call_context(
            "dm-ignores-call",
            "dm",
            Some(call_context_child("", "invalid", "invalid")),
        ),
    )
    .await;
    let alerts = sender.calls_for(TOKEN_B);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].push_type, ApnsPushType::Alert);
    assert!(alerts[0].payload["waddle"].get("call").is_none());
}

#[tokio::test]
async fn voip_node_skips_non_apns_devices_without_retry_or_disable() {
    let sender = FakeApnsSender::default();
    let store = store_with_apns(sender.clone(), counting_tokens()).await;
    let owner = owner();
    let node = store
        .ensure_node(&owner, "p4x.waddle.social.voip")
        .await
        .expect("VoIP node");
    register_apple_device(&store, &owner, node.node(), "iphone", "prod", TOKEN_A).await;
    for (device, platform) in [
        ("web", PushDevicePlatform::Web),
        ("android", PushDevicePlatform::Fcm),
    ] {
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new(device, node.node(), platform, "prod"),
            )
            .await
            .expect("non-APNs device on VoIP node");
    }
    publish(
        &store,
        &owner,
        node.node(),
        item_with_call_context(
            "mixed-call",
            "call",
            Some(call_context_child(
                "mixed-sid",
                "false",
                &(SystemApnsClock.now_unix_seconds() + 45).to_string(),
            )),
        ),
    )
    .await;
    assert_eq!(sender.calls_for(TOKEN_A).len(), 1);
    assert_eq!(job_status(&store, "mixed-call").await, "published");
    assert_eq!(
        attempt_statuses(&store, node.node()).await,
        vec![
            ("android".to_owned(), "not-applicable".to_owned()),
            (
                "iphone".to_owned(),
                ATTEMPT_STATUS_APNS_DELIVERED.to_owned()
            ),
            ("web".to_owned(), "not-applicable".to_owned()),
        ]
    );
    for device in ["web", "android", "iphone"] {
        assert_eq!(
            device_status(&store, node.node(), device).await,
            DEVICE_STATUS_ACTIVE
        );
    }
    store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("terminal jobs stay idle");
    assert_eq!(sender.calls_for(TOKEN_A).len(), 1);
}

#[cfg(feature = "test-support")]
async fn notification_wake_state(
    fixture: &ingress_support::IngressFixture,
    sender: Arc<dyn ApnsSender>,
) -> Arc<waddle_server::test_support::WebSocketState> {
    use waddle_server::db::{DatabaseConfig, DatabasePool, PoolConfig};
    use waddle_server::test_support::{seed_local_account, websocket_state_with_ingress};

    let pool = Arc::new(
        DatabasePool::try_new(
            DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
            PoolConfig,
        )
        .await
        .expect("shared database pool"),
    );
    let mut state = websocket_state_with_ingress(pool, Arc::new(fixture.authority().await)).await;
    seed_local_account(&state, "juliet").await;
    let push = state
        .deps
        .protocol
        .push_service
        .as_ref()
        .clone()
        .with_inbox_storage(state.deps.protocol.inbox_storage.clone())
        .with_apns_provider(
            counting_tokens(),
            sender,
            ApnsTopic::parse(BUNDLE_ID).expect("bundle topic"),
        )
        .expect("APNs topics");
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    let node = push
        .ensure_node(&recipient, "p4x.waddle.social.voip")
        .await
        .expect("VoIP node");
    register_apple_device(&push, &recipient, node.node(), "phone", "prod", TOKEN_A).await;
    push.register_first_party_node_for_owner(&recipient, PUSH_SERVICE_JID, node.node(), None)
        .await
        .expect("enable VoIP push");
    Arc::get_mut(&mut state)
        .expect("unshared test state")
        .deps
        .protocol
        .push_service = Arc::new(push);
    state
}

#[cfg(feature = "test-support")]
async fn wait_for_provider_sends(sender: &FakeApnsSender, expected: usize) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while sender.calls().len() < expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("call reaches the provider without a timer tick");
}

#[cfg(feature = "test-support")]
async fn stop_notification_wake_state(
    state: Arc<waddle_server::test_support::WebSocketState>,
    loops: [tokio::task::JoinHandle<()>; 2],
) {
    for task in loops {
        task.abort();
        let _ = task.await;
    }
    assert!(
        state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(15))
            .await
    );
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn committed_call_wakes_both_notification_loops_with_stored_permit() {
    use waddle_server::test_support::spawn_notification_loops;
    use waddle_xmpp::xep::xep0353::CallOffer;

    let fixture = ingress_support::IngressFixture::sqlite().await;
    let sender = FakeApnsSender::default();
    let state = notification_wake_state(&fixture, Arc::new(sender.clone())).await;
    let dm = offline_submission(&fixture, "dm-pending-during-call-wake", None);
    execute_offline_notification(
        &fixture,
        &state.deps.protocol.ingress,
        state.as_ref(),
        &dm,
        1,
    )
    .await;
    let call = offline_submission(
        &fixture,
        "wake-call-before-start",
        Some(CallOffer::audio_only()),
    );
    execute_offline_notification(
        &fixture,
        &state.deps.protocol.ingress,
        state.as_ref(),
        &call,
        2,
    )
    .await;
    assert_eq!(
        state
            .deps
            .protocol
            .notification_outbox
            .pending_candidates(16)
            .await
            .expect("durably committed candidate")
            .len(),
        2
    );
    assert_eq!(fixture.count("push_publish_jobs").await, 0);

    // Both first ticks are discarded; only the permit stored by commit can
    // start this chain with timers set well beyond the assertion deadline.
    let loops = spawn_notification_loops(&state, Duration::from_secs(3_600));
    wait_for_provider_sends(&sender, 1).await;
    let calls = sender.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].push_type, ApnsPushType::Voip);
    assert_eq!(calls[0].payload["waddle"]["item"], "wake-call-before-start");
    assert_eq!(fixture.count("notification_outbox").await, 1);
    assert_eq!(fixture.count("push_publish_jobs").await, 1);
    let pending = state
        .deps
        .protocol
        .notification_outbox
        .pending_candidates(16)
        .await
        .expect("DM remains pending after call wake");
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].archive_stanza_id().id,
        "dm-pending-during-call-wake"
    );

    // Re-arm with a short timer after consuming the stored call wake. With
    // no new commits or wakes, only a timed sweep can drain the remaining DM.
    for task in loops {
        task.abort();
        let _ = task.await;
    }
    let loops = spawn_notification_loops(&state, Duration::from_millis(25));
    tokio::time::timeout(Duration::from_secs(30), async {
        while !state
            .deps
            .protocol
            .notification_outbox
            .pending_candidates(16)
            .await
            .expect("pending candidates after timer")
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("next timed sweep drains the DM");
    stop_notification_wake_state(state, loops).await;
    fixture.close().await;
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn committed_dm_does_not_wake_notification_outbox_loop() {
    use waddle_server::test_support::spawn_notification_loops;

    let fixture = ingress_support::IngressFixture::sqlite().await;
    let sender = FakeApnsSender::default();
    let state = notification_wake_state(&fixture, Arc::new(sender.clone())).await;
    let loops = spawn_notification_loops(&state, Duration::from_secs(3_600));
    let dm = offline_submission(&fixture, "dm-waits-for-timer", None);
    execute_offline_notification(
        &fixture,
        &state.deps.protocol.ingress,
        state.as_ref(),
        &dm,
        1,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        state
            .deps
            .protocol
            .notification_outbox
            .pending_candidates(16)
            .await
            .expect("pending DM")
            .len(),
        1,
        "ordinary alert batching waits for the timer"
    );
    assert_eq!(fixture.count("notification_outbox").await, 0);
    assert_eq!(fixture.count("push_publish_jobs").await, 0);
    assert!(sender.calls().is_empty());
    stop_notification_wake_state(state, loops).await;
    fixture.close().await;
}

#[cfg(feature = "test-support")]
struct GatedApnsSender {
    inner: FakeApnsSender,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    gated: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "test-support")]
impl ApnsSender for GatedApnsSender {
    fn send(
        &self,
        request: ApnsRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = ApnsOutcome> + Send + '_>> {
        let outcome = self.inner.send(request);
        Box::pin(async move {
            if !self.gated.swap(true, Ordering::SeqCst) {
                self.started.notify_one();
                self.release.notified().await;
            }
            outcome.await
        })
    }
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn call_wake_during_provider_sweep_is_not_lost() {
    use waddle_server::test_support::spawn_notification_loops;
    use waddle_xmpp::xep::xep0353::CallOffer;

    let fixture = ingress_support::IngressFixture::sqlite().await;
    let captured = FakeApnsSender::default();
    let sender = Arc::new(GatedApnsSender {
        inner: captured.clone(),
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        gated: std::sync::atomic::AtomicBool::new(false),
    });
    let state = notification_wake_state(&fixture, sender.clone()).await;
    let loops = spawn_notification_loops(&state, Duration::from_secs(3_600));
    let first = offline_submission(&fixture, "first-wake-call", Some(CallOffer::audio_only()));
    execute_offline_notification(
        &fixture,
        &state.deps.protocol.ingress,
        state.as_ref(),
        &first,
        1,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), sender.started.notified())
        .await
        .expect("first provider sweep enters gated send");

    let second = offline_submission(&fixture, "second-wake-call", Some(CallOffer::audio_only()));
    execute_offline_notification(
        &fixture,
        &state.deps.protocol.ingress,
        state.as_ref(),
        &second,
        2,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), async {
        // Publication bookkeeping follows notify_one, so this also proves the
        // second wake happened while the first provider sweep was blocked.
        while fixture
            .count("notification_outbox WHERE status = 'published'")
            .await
            < 2
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second call handed to provider queue while first sweep is busy");
    assert_eq!(
        captured.calls().len(),
        1,
        "first send still awaiting provider outcome"
    );
    sender.release.notify_one();
    wait_for_provider_sends(&captured, 2).await;
    let items: Vec<_> = captured
        .calls()
        .into_iter()
        .map(|request| request.payload["waddle"]["item"].clone())
        .collect();
    assert!(items.contains(&serde_json::json!("first-wake-call")));
    assert!(items.contains(&serde_json::json!("second-wake-call")));
    stop_notification_wake_state(state, loops).await;
    fixture.close().await;
}

#[cfg(feature = "test-support")]
struct GatedBlockingStorage {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    gated: std::sync::atomic::AtomicBool,
}

#[cfg(feature = "test-support")]
#[async_trait::async_trait]
impl waddle_xmpp::xep::xep0191::BlockingStorage for GatedBlockingStorage {
    async fn list_blocked_jids(
        &self,
        _user: &BareJid,
    ) -> Result<Vec<BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        if !self.gated.swap(true, Ordering::SeqCst) {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(Vec::new())
    }
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn call_wake_during_outbox_sweep_is_not_lost() {
    use waddle_server::test_support::spawn_notification_loops;
    use waddle_xmpp::xep::xep0353::CallOffer;

    let fixture = ingress_support::IngressFixture::sqlite().await;
    let sender = FakeApnsSender::default();
    let mut state = notification_wake_state(&fixture, Arc::new(sender.clone())).await;
    let blocking = Arc::new(GatedBlockingStorage {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        gated: std::sync::atomic::AtomicBool::new(false),
    });
    Arc::get_mut(&mut state)
        .expect("unshared test state")
        .deps
        .protocol
        .blocking_storage = blocking.clone();
    let loops = spawn_notification_loops(&state, Duration::from_secs(3_600));
    let first = offline_submission(&fixture, "first-outbox-wake", Some(CallOffer::audio_only()));
    execute_offline_notification(
        &fixture,
        &state.deps.protocol.ingress,
        state.as_ref(),
        &first,
        1,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), blocking.started.notified())
        .await
        .expect("outbox sweep reaches policy read after taking its candidate snapshot");

    // The second commit arrives after the first sweep's candidate snapshot.
    // Its stored permit must cause a second sweep, without another timer tick.
    let second = offline_submission(
        &fixture,
        "second-outbox-wake",
        Some(CallOffer::audio_only()),
    );
    execute_offline_notification(
        &fixture,
        &state.deps.protocol.ingress,
        state.as_ref(),
        &second,
        2,
    )
    .await;
    assert_eq!(
        state
            .deps
            .protocol
            .notification_outbox
            .pending_candidates(16)
            .await
            .expect("both candidates committed while sweep is blocked")
            .len(),
        2
    );
    assert_eq!(fixture.count("push_publish_jobs").await, 0);
    blocking.release.notify_one();
    wait_for_provider_sends(&sender, 2).await;
    assert!(state
        .deps
        .protocol
        .notification_outbox
        .pending_candidates(16)
        .await
        .expect("candidate drain")
        .is_empty());
    stop_notification_wake_state(state, loops).await;
    fixture.close().await;
}
