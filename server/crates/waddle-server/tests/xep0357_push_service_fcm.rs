//! XEP-0357 Push Service → FCM delivery tests (#530).
//!
//! The Push Service (`push.<domain>`) is the only holder of FCM
//! registration tokens. These tests drive a node → Android-device
//! fan-out through the store's public API with a fake `FcmSender` that
//! records every request and a fake OAuth token source, so they cover:
//!
//! - the XEP-0357 registration boundary: the user server keeps only the
//!   service JID, node and publish-options, and refuses FCM credentials
//!   smuggled into publish-options;
//! - node-to-FCM-device fan-out carrying the RFC 8291 ciphertext of the
//!   v1 envelope, which decrypts with each device's own keys and leaves
//!   no conversation JID, sender or body in what Google receives;
//! - FCM rejection disabling only the rejected `push_devices` row
//!   (§6), and the last-device §6 path;
//! - provider-wide failures (not configured, refused credentials, a
//!   bare `404`, a foreign app id) that never disable a device;
//! - access-token refresh on `401`, token-endpoint outages that are
//!   retried without marking the send uncertain, and transient retries.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes128Gcm;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hkdf::Hkdf;
use jid::BareJid;
use minidom::Element;
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use sha2::Sha256;
use waddle_server::db::{Database, IntoParams, Rows};
use waddle_server::db_params;
use waddle_server::push_registrations::DatabasePushRegistrationStore;
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::push::fcm::{
    FcmAccessToken, FcmAccessTokenSource, FcmAppId, FcmErrorCode, FcmErrorDetail, FcmOutcome,
    FcmProjectId, FcmRequest, FcmSender, FcmTokenError, FcmTransient, GoogleRpcStatus,
    OAuthErrorCode,
};
use waddle_xmpp::push::types::TransientFailure;
use waddle_xmpp::push::{PushSubscription, PushSubscriptionStore};
use waddle_xmpp::xep::xep0004::NS_DATA_FORMS;
use waddle_xmpp::xep::xep0357::NS_PUSH;

const APP_ID: &str = "social.waddle.android";
const PROJECT_ID: &str = "waddle-test";
const PUSH_SERVICE_JID: &str = "push.example.com";
/// Mirrors `DEVICE_STATUS_*` in `waddle_server::push_service`.
const DEVICE_STATUS_ACTIVE: &str = "active";
const DEVICE_STATUS_DISABLED: &str = "disabled";
/// Mirror `fcm_dispatch::ATTEMPT_STATUS_FCM_*`.
const FCM_DELIVERED: &str = "fcm-delivered";
const FCM_GONE: &str = "fcm-gone";
const FCM_INVALID_TOKEN: &str = "fcm-invalid-token";
const FCM_REJECTED: &str = "fcm-rejected";
const FCM_NOT_CONFIGURED: &str = "fcm-not-configured";
const FCM_PROVIDER_AUTH: &str = "fcm-provider-auth";
const FCM_APP_ID_MISMATCH: &str = "fcm-app-id-mismatch";
const FCM_TOKEN_TRANSIENT: &str = "fcm-token-transient";
const FCM_TRANSIENT: &str = "fcm-transient";
/// RFC 8291 §3.4 / RFC 8188 §2 constants for the receiver side.
const WEBPUSH_INFO_PREFIX: &[u8] = b"WebPush: info\0";
const CEK_INFO: &[u8] = b"Content-Encoding: aes128gcm\0";
const NONCE_INFO: &[u8] = b"Content-Encoding: nonce\0";
const AES128GCM_HEADER_LEN: usize = 16 + 4 + 1 + 65;

/// One Android client: its registration token and the RFC 8291 keys it
/// generated, private half included so the test can decrypt.
struct AndroidDevice {
    token: String,
    secret: p256::SecretKey,
    auth: [u8; 16],
}

impl AndroidDevice {
    fn new(token: &str) -> Self {
        Self {
            token: token.to_string(),
            secret: p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng),
            auth: rand::random(),
        }
    }

    fn p256dh(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.secret.public_key().to_encoded_point(false).as_bytes())
    }

    fn registration(&self, device_id: &str, node: &str) -> PushDeviceRegistration {
        PushDeviceRegistration::new(device_id, node, PushDevicePlatform::Fcm, "prod")
            .with_provider_token(Some(self.token.clone()))
            .with_provider_key_material(Some(self.p256dh()))
            .with_provider_auth(Some(URL_SAFE_NO_PAD.encode(self.auth)))
    }

    /// RFC 8291 receiver: recover the plaintext envelope.
    fn decrypt(&self, body: &[u8]) -> Vec<u8> {
        assert!(body.len() > AES128GCM_HEADER_LEN, "aes128gcm record");
        let salt = &body[..16];
        assert_eq!(body[20], 65, "keyid is the sender's P-256 public key");
        let as_public_bytes = &body[21..AES128GCM_HEADER_LEN];
        let as_public = p256::PublicKey::from_encoded_point(
            &p256::EncodedPoint::from_bytes(as_public_bytes).expect("encoded point"),
        )
        .expect("sender public key");
        let shared =
            p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), as_public.as_affine());
        let mut key_info = WEBPUSH_INFO_PREFIX.to_vec();
        key_info.extend_from_slice(self.secret.public_key().to_encoded_point(false).as_bytes());
        key_info.extend_from_slice(as_public_bytes);
        let mut ikm = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&self.auth), shared.raw_secret_bytes())
            .expand(&key_info, &mut ikm)
            .expect("ikm");
        let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
        let mut cek = [0u8; 16];
        prk.expand(CEK_INFO, &mut cek).expect("cek");
        let mut nonce = [0u8; 12];
        prk.expand(NONCE_INFO, &mut nonce).expect("nonce");
        let padded = Aes128Gcm::new_from_slice(&cek)
            .expect("cipher")
            .decrypt((&nonce).into(), &body[AES128GCM_HEADER_LEN..])
            .expect("decrypts with the device's own keys");
        let end = padded
            .iter()
            .rposition(|byte| *byte != 0)
            .expect("padding delimiter");
        assert_eq!(padded[end], 2, "RFC 8188 last-record delimiter");
        padded[..end].to_vec()
    }
}

/// One request as the fake FCM endpoint saw it.
#[derive(Debug, Clone)]
struct CapturedRequest {
    project_id: String,
    access_token: String,
    raw_body: String,
    body: serde_json::Value,
}

impl CapturedRequest {
    fn registration_token(&self) -> &str {
        self.body["message"]["token"].as_str().expect("token")
    }

    fn ciphertext(&self) -> Vec<u8> {
        URL_SAFE_NO_PAD
            .decode(
                self.body["message"]["data"]["payload"]
                    .as_str()
                    .expect("payload data field"),
            )
            .expect("base64url ciphertext")
    }
}

/// Fake `FcmSender`: per-token outcome sequences (the last one repeats;
/// unknown tokens get `Sent`) and a log of every request.
#[derive(Clone, Default)]
struct FakeFcmSender {
    outcomes: Arc<Vec<(&'static str, Vec<FcmOutcome>)>>,
    calls: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl FakeFcmSender {
    fn with_outcomes(outcomes: Vec<(&'static str, Vec<FcmOutcome>)>) -> Self {
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
            .filter(|call| call.registration_token() == token)
            .collect()
    }
}

impl FcmSender for FakeFcmSender {
    fn send(
        &self,
        request: FcmRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = FcmOutcome> + Send + '_>> {
        let raw_body =
            String::from_utf8(request.message.as_slice().to_vec()).expect("UTF-8 JSON body");
        let captured = CapturedRequest {
            project_id: request.project_id.as_str().to_string(),
            access_token: request.access_token.as_str().to_string(),
            body: serde_json::from_str(&raw_body).expect("JSON body"),
            raw_body,
        };
        let token = captured.registration_token().to_string();
        let mut calls = self.calls.lock().expect("calls lock");
        let prior = calls
            .iter()
            .filter(|call| call.registration_token() == token)
            .count();
        let outcome = self
            .outcomes
            .iter()
            .find(|(candidate, _)| *candidate == token)
            .map(|(_, sequence)| sequence[prior.min(sequence.len() - 1)].clone())
            .unwrap_or(FcmOutcome::Sent);
        calls.push(captured);
        drop(calls);
        Box::pin(async move { outcome })
    }
}

/// Fake OAuth source: mints `ya29.token-N`, bumping N on invalidation of
/// the current token, or fails with a fixed error.
struct CountingTokens {
    failure: Option<FcmTokenError>,
    generation: AtomicUsize,
    fetches: AtomicUsize,
    invalidations: AtomicUsize,
}

impl CountingTokens {
    fn new() -> Arc<Self> {
        Self::failing_with(None)
    }

    fn failing_with(failure: Option<FcmTokenError>) -> Arc<Self> {
        Arc::new(Self {
            failure,
            generation: AtomicUsize::new(1),
            fetches: AtomicUsize::new(0),
            invalidations: AtomicUsize::new(0),
        })
    }

    fn current_token(&self) -> String {
        format!("ya29.token-{}", self.generation.load(Ordering::SeqCst))
    }
}

impl FcmAccessTokenSource for CountingTokens {
    fn current(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<FcmAccessToken, FcmTokenError>> + Send + '_>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        let result = match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(FcmAccessToken::new(self.current_token())),
        };
        Box::pin(async move { result })
    }

    fn invalidate(&self, rejected: &FcmAccessToken) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
        if rejected.as_str() == self.current_token() {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }
}

async fn bare_store() -> DatabasePushServiceStore {
    let store = DatabasePushServiceStore::new_with_secret_key(
        Database::in_memory("push-service-fcm")
            .await
            .expect("push service db"),
        b"waddle-push-service-test-secret-key",
    )
    .await
    .expect("push service store");
    // The user server's XEP-0357 registration table lives in the same
    // database; creating its store creates the table.
    DatabasePushRegistrationStore::new(store.database())
        .await
        .expect("registration store");
    store
}

async fn store_with_fcm(
    sender: FakeFcmSender,
    tokens: Arc<CountingTokens>,
) -> DatabasePushServiceStore {
    bare_store().await.with_fcm_provider(
        tokens,
        Arc::new(sender),
        FcmProjectId::parse(PROJECT_ID).expect("project id"),
        FcmAppId::parse(APP_ID).expect("app id"),
    )
}

fn owner() -> BareJid {
    "alice@example.com".parse().expect("owner jid")
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

/// XEP-0357 §4 notification with the summary form (carrying
/// `last-message-sender` / `last-message-body`, which XEP-0357 allows)
/// and the Waddle routing context.
fn notification_item(item_id: &str, class: &str, message_count: u32) -> PubSubItem {
    let summary = Element::builder("x", NS_DATA_FORMS)
        .append(data_form_field("FORM_TYPE", "urn:xmpp:push:summary"))
        .append(data_form_field("message-count", &message_count.to_string()))
        .append(data_form_field(
            "last-message-sender",
            "mallory@example.com/phone",
        ))
        .append(data_form_field(
            "last-message-body",
            "the launch code is 0000",
        ))
        .build();
    let context = Element::builder("context", "urn:waddle:push:context:0")
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            "bob@example.com",
        )
        .attr(minidom::rxml::xml_ncname!("class").to_owned(), class)
        .attr(
            minidom::rxml::xml_ncname!("stanza-id").to_owned(),
            "stanza-42",
        )
        .build();
    let notification = Element::builder("notification", NS_PUSH)
        .append(summary)
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

async fn job_uncertain_send(store: &DatabasePushServiceStore, item_id: &str) -> i64 {
    let mut rows = query(
        store,
        "SELECT uncertain_send FROM push_publish_jobs WHERE item_id = ?",
        db_params![item_id],
    )
    .await;
    let row = rows.next().await.expect("row").expect("row present");
    row.get::<i64>(0).expect("uncertain_send")
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
    DatabasePushRegistrationStore::new(store.database())
        .await
        .expect("registration store")
        .get_for_user(owner.to_string().as_str())
        .await
        .expect("registrations")
        .len()
}

async fn publish(store: &DatabasePushServiceStore, owner: &BareJid, node: &str, item: PubSubItem) {
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

/// Register `devices` on a fresh Android node and enable it at the user
/// server, returning the node id.
async fn android_node(
    store: &DatabasePushServiceStore,
    owner: &BareJid,
    app_id: &str,
    devices: &[(&str, &AndroidDevice)],
) -> String {
    let node = store
        .ensure_node(owner, app_id)
        .await
        .expect("node")
        .node()
        .to_string();
    for (device_id, device) in devices {
        store
            .upsert_device(owner, device.registration(device_id, &node))
            .await
            .expect("android device");
    }
    store
        .register_first_party_node_for_owner(owner, PUSH_SERVICE_JID, &node, None)
        .await
        .expect("XEP-0357 registration");
    node
}

#[tokio::test]
async fn xep0357_user_server_stores_only_the_service_jid_and_node_for_android() {
    let store = store_with_fcm(FakeFcmSender::default(), CountingTokens::new()).await;
    let owner = owner();
    let phone = AndroidDevice::new("fcm-token-phone");
    let node = android_node(&store, &owner, APP_ID, &[("phone", &phone)]).await;

    // The XEP-0357 registration row carries the service JID and node, and
    // no column of it holds any of the device's FCM credentials.
    let registrations = DatabasePushRegistrationStore::new(store.database())
        .await
        .expect("registration store")
        .get_for_user(owner.to_string().as_str())
        .await
        .expect("registrations");
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].service_jid, PUSH_SERVICE_JID);
    assert_eq!(registrations[0].node.as_deref(), Some(node.as_str()));
    assert!(registrations[0].endpoint.is_none());
    assert!(registrations[0].p256dh.is_none());
    assert!(registrations[0].auth_key.is_none());
    let mut rows = query(&store, "SELECT * FROM push_registrations", ()).await;
    let columns = rows.column_count();
    let row = rows.next().await.expect("row").expect("registration row");
    for column in 0..columns {
        if let Ok(Some(value)) = row.get::<Option<String>>(column) {
            for secret in [
                phone.token.clone(),
                phone.p256dh(),
                URL_SAFE_NO_PAD.encode(phone.auth),
            ] {
                assert!(
                    !value.contains(&secret),
                    "push_registrations column {column} leaked an FCM credential"
                );
            }
        }
    }

    // FCM credentials smuggled into XEP-0357 publish-options are refused
    // at the user server, so they can never be persisted there.
    for field in ["fcm-token", "fcm-p256dh", "fcm-auth"] {
        let options = Element::builder("x", NS_DATA_FORMS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
            .append(data_form_field(
                "FORM_TYPE",
                waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS,
            ))
            .append(data_form_field(field, "smuggled-secret"))
            .build();
        let error = DatabasePushRegistrationStore::new(store.database())
            .await
            .expect("registration store")
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: PUSH_SERVICE_JID.to_string(),
                node: Some(node.clone()),
                publish_options: Some(options),
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect_err("FCM credentials must not enter the user server");
        assert!(
            error.to_string().contains("provider credential fields"),
            "{field}: {error}"
        );
    }
}

#[tokio::test]
async fn xep0357_node_fans_out_to_every_android_device_as_ciphertext_only() {
    let sender = FakeFcmSender::default();
    let tokens = CountingTokens::new();
    let store = store_with_fcm(sender.clone(), Arc::clone(&tokens)).await;
    let owner = owner();
    let phone = AndroidDevice::new("fcm-token-phone");
    let tablet = AndroidDevice::new("fcm-token-tablet");
    let node = android_node(
        &store,
        &owner,
        APP_ID,
        &[("phone", &phone), ("tablet", &tablet)],
    )
    .await;

    publish(
        &store,
        &owner,
        &node,
        notification_item("fanout-1", "dm", 4),
    )
    .await;

    let calls = sender.calls();
    assert_eq!(calls.len(), 2, "one FCM request per active Android device");
    assert_eq!(
        tokens.fetches.load(Ordering::SeqCst),
        1,
        "one access token is fetched before the fan-out and shared"
    );
    for device in [&phone, &tablet] {
        let call = sender.calls_for(&device.token);
        assert_eq!(call.len(), 1, "exactly one request for {}", device.token);
        let call = &call[0];
        assert_eq!(call.project_id, PROJECT_ID);
        assert_eq!(call.access_token, "ya29.token-1");
        // Data-only: no `notification` block Google would render, no
        // collapse key, DM urgency and a 7-day TTL.
        let message = call.body["message"].as_object().expect("message object");
        let mut keys = message.keys().map(String::as_str).collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(keys, ["android", "data", "token"]);
        assert_eq!(
            call.body["message"]["android"],
            serde_json::json!({ "priority": "HIGH", "ttl": "604800s" })
        );
        let data = message["data"].as_object().expect("data object");
        assert_eq!(data.keys().collect::<Vec<_>>(), ["payload"]);
        // Google sees ciphertext only: no conversation, owner, sender,
        // body or item id in the request.
        for leaked in [
            "bob@example.com",
            "alice@example.com",
            "mallory",
            "launch code",
            "stanza-42",
            "personal_mention",
            "conversation",
        ] {
            assert!(
                !call.raw_body.contains(leaked),
                "FCM request leaked {leaked:?}: {}",
                call.raw_body
            );
        }
        // The device's own keys recover the same v1 envelope Web Push
        // carries: routing context and unread count, no sender or body.
        let envelope: serde_json::Value =
            serde_json::from_slice(&device.decrypt(&call.ciphertext())).expect("v1 envelope");
        assert_eq!(
            envelope,
            serde_json::json!({
                "v": 1,
                "class": "dm",
                "conversation": "bob@example.com",
                "item": "stanza-42",
                "unread": 4,
            })
        );
    }
    assert_ne!(
        sender.calls_for(&phone.token)[0].ciphertext(),
        sender.calls_for(&tablet.token)[0].ciphertext(),
        "each device gets its own encryption"
    );
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![
            ("phone".to_string(), FCM_DELIVERED.to_string()),
            ("tablet".to_string(), FCM_DELIVERED.to_string()),
        ]
    );
    assert_eq!(job_status(&store, "fanout-1").await, "published");
}

#[tokio::test]
async fn fcm_priority_and_ttl_follow_the_notification_class() {
    let sender = FakeFcmSender::default();
    let store = store_with_fcm(sender.clone(), CountingTokens::new()).await;
    let owner = owner();
    let phone = AndroidDevice::new("fcm-token-phone");
    let node = android_node(&store, &owner, APP_ID, &[("phone", &phone)]).await;

    publish(
        &store,
        &owner,
        &node,
        notification_item("broadcast-1", "notify_all", 1),
    )
    .await;

    let calls = sender.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].body["message"]["android"],
        serde_json::json!({ "priority": "NORMAL", "ttl": "14400s" }),
        "@everyone broadcasts let Android defer delivery"
    );
}

#[tokio::test]
async fn xep0357_fcm_unregistered_disables_only_that_device_not_the_registration() {
    let gone = AndroidDevice::new("fcm-token-gone");
    let live = AndroidDevice::new("fcm-token-live");
    let sender = FakeFcmSender::with_outcomes(vec![(
        "fcm-token-gone",
        vec![FcmOutcome::Unregistered {
            status: 404,
            detail: FcmErrorDetail {
                status: Some(GoogleRpcStatus::NotFound),
                error_code: Some(FcmErrorCode::Unregistered),
                ..FcmErrorDetail::default()
            },
        }],
    )]);
    let store = store_with_fcm(sender.clone(), CountingTokens::new()).await;
    let owner = owner();
    let node = android_node(
        &store,
        &owner,
        APP_ID,
        &[("gone-phone", &gone), ("live-phone", &live)],
    )
    .await;
    assert_eq!(enabled_registrations(&store, &owner).await, 1);

    publish(
        &store,
        &owner,
        &node,
        notification_item("reject-1", "dm", 1),
    )
    .await;

    assert_eq!(
        device_status(&store, &node, "gone-phone").await,
        DEVICE_STATUS_DISABLED,
        "FCM UNREGISTERED disables the rejected device row"
    );
    assert_eq!(
        device_status(&store, &node, "live-phone").await,
        DEVICE_STATUS_ACTIVE,
        "the sibling device keeps receiving pushes"
    );
    assert_eq!(
        enabled_registrations(&store, &owner).await,
        1,
        "XEP-0357 §6: the registration stays enabled while a device remains"
    );
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![
            ("gone-phone".to_string(), FCM_GONE.to_string()),
            ("live-phone".to_string(), FCM_DELIVERED.to_string()),
        ]
    );
    let last_error = scalar_string(
        &store,
        "SELECT last_error FROM push_delivery_attempts WHERE device_id = ?",
        db_params!["gone-phone"],
    )
    .await;
    assert_eq!(last_error, "unregistered HTTP 404 NOT_FOUND/UNREGISTERED");

    // The disabled device is no longer part of the fan-out.
    publish(
        &store,
        &owner,
        &node,
        notification_item("reject-2", "dm", 2),
    )
    .await;
    assert_eq!(sender.calls_for(&gone.token).len(), 1);
    assert_eq!(sender.calls_for(&live.token).len(), 2);
}

#[tokio::test]
async fn xep0357_invalid_token_on_the_last_android_device_disables_the_registration() {
    let only = AndroidDevice::new("fcm-token-only");
    let sender = FakeFcmSender::with_outcomes(vec![(
        "fcm-token-only",
        vec![FcmOutcome::InvalidToken {
            status: 400,
            detail: FcmErrorDetail {
                status: Some(GoogleRpcStatus::InvalidArgument),
                error_code: Some(FcmErrorCode::InvalidArgument),
                token_field_violation: true,
                ..FcmErrorDetail::default()
            },
        }],
    )]);
    let store = store_with_fcm(sender, CountingTokens::new()).await;
    let owner = owner();
    let node = android_node(&store, &owner, APP_ID, &[("only-phone", &only)]).await;

    publish(&store, &owner, &node, notification_item("last-1", "dm", 1)).await;

    assert_eq!(
        device_status(&store, &node, "only-phone").await,
        DEVICE_STATUS_DISABLED
    );
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![("only-phone".to_string(), FCM_INVALID_TOKEN.to_string())]
    );
    assert_eq!(
        enabled_registrations(&store, &owner).await,
        0,
        "XEP-0357 §6: no deliverable device remains, so the registration is disabled"
    );
}

/// FCM's documented reply to a malformed registration token: HTTP 400,
/// `INVALID_ARGUMENT`, and no field named.
fn unattributed_invalid_argument() -> FcmOutcome {
    FcmOutcome::UnattributedInvalidArgument {
        status: 400,
        detail: FcmErrorDetail {
            status: Some(GoogleRpcStatus::InvalidArgument),
            error_code: Some(FcmErrorCode::InvalidArgument),
            ..FcmErrorDetail::default()
        },
    }
}

#[tokio::test]
async fn unattributed_invalid_argument_keeps_the_device_before_fcm_accepted_anything() {
    // Nothing has proven the request shape yet: a bug in it must not
    // disable every Android device, so the device is kept.
    let only = AndroidDevice::new("fcm-token-garbled");
    let sender = FakeFcmSender::with_outcomes(vec![(
        "fcm-token-garbled",
        vec![unattributed_invalid_argument()],
    )]);
    let store = store_with_fcm(sender, CountingTokens::new()).await;
    let owner = owner();
    let node = android_node(&store, &owner, APP_ID, &[("only-phone", &only)]).await;

    publish(
        &store,
        &owner,
        &node,
        notification_item("unproven-1", "dm", 1),
    )
    .await;

    assert_eq!(
        device_status(&store, &node, "only-phone").await,
        DEVICE_STATUS_ACTIVE
    );
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![("only-phone".to_string(), FCM_REJECTED.to_string())]
    );
    assert_eq!(enabled_registrations(&store, &owner).await, 1);
}

#[tokio::test]
async fn unattributed_invalid_argument_disables_the_device_once_fcm_accepted_a_message() {
    let good = AndroidDevice::new("fcm-token-good");
    let garbled = AndroidDevice::new("fcm-token-garbled");
    let sender = FakeFcmSender::with_outcomes(vec![(
        "fcm-token-garbled",
        vec![unattributed_invalid_argument()],
    )]);
    let store = store_with_fcm(sender, CountingTokens::new()).await;

    // Another account's delivery proves this server's request shape.
    let carol: BareJid = "carol@example.com".parse().expect("carol jid");
    let carol_node = android_node(&store, &carol, APP_ID, &[("carol-phone", &good)]).await;
    publish(
        &store,
        &carol,
        &carol_node,
        notification_item("proof-1", "dm", 1),
    )
    .await;
    assert_eq!(
        attempt_statuses(&store, &carol_node).await,
        vec![("carol-phone".to_string(), FCM_DELIVERED.to_string())]
    );

    let owner = owner();
    let node = android_node(&store, &owner, APP_ID, &[("only-phone", &garbled)]).await;
    publish(
        &store,
        &owner,
        &node,
        notification_item("garbled-1", "dm", 1),
    )
    .await;

    assert_eq!(
        device_status(&store, &node, "only-phone").await,
        DEVICE_STATUS_DISABLED,
        "with the request shape proven, FCM refused the token itself"
    );
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![("only-phone".to_string(), FCM_INVALID_TOKEN.to_string())]
    );
    assert_eq!(
        enabled_registrations(&store, &owner).await,
        0,
        "XEP-0357 §6: no deliverable device remains, so the registration is disabled"
    );
}

#[tokio::test]
async fn unconfigured_fcm_keeps_devices_and_fails_the_job() {
    let store = bare_store().await;
    let owner = owner();
    let phone = AndroidDevice::new("fcm-token-phone");
    let node = android_node(&store, &owner, APP_ID, &[("phone", &phone)]).await;

    publish(
        &store,
        &owner,
        &node,
        notification_item("unconfigured-1", "dm", 1),
    )
    .await;

    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![("phone".to_string(), FCM_NOT_CONFIGURED.to_string())]
    );
    assert_eq!(
        device_status(&store, &node, "phone").await,
        DEVICE_STATUS_ACTIVE
    );
    assert_eq!(enabled_registrations(&store, &owner).await, 1);
    assert_eq!(job_status(&store, "unconfigured-1").await, "failed");
}

#[tokio::test]
async fn bare_404_from_fcm_is_provider_auth_and_never_disables_devices() {
    let phone = AndroidDevice::new("fcm-token-phone");
    let tablet = AndroidDevice::new("fcm-token-tablet");
    let unknown_project = FcmOutcome::ProviderAuth {
        status: 404,
        detail: FcmErrorDetail::default(),
    };
    let sender = FakeFcmSender::with_outcomes(vec![
        ("fcm-token-phone", vec![unknown_project.clone()]),
        ("fcm-token-tablet", vec![unknown_project]),
    ]);
    let store = store_with_fcm(sender, CountingTokens::new()).await;
    let owner = owner();
    let node = android_node(
        &store,
        &owner,
        APP_ID,
        &[("phone", &phone), ("tablet", &tablet)],
    )
    .await;

    publish(
        &store,
        &owner,
        &node,
        notification_item("project-1", "dm", 1),
    )
    .await;

    for device_id in ["phone", "tablet"] {
        assert_eq!(
            device_status(&store, &node, device_id).await,
            DEVICE_STATUS_ACTIVE,
            "a wrong project id is not the device's fault"
        );
    }
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![
            ("phone".to_string(), FCM_PROVIDER_AUTH.to_string()),
            ("tablet".to_string(), FCM_PROVIDER_AUTH.to_string()),
        ]
    );
    assert_eq!(enabled_registrations(&store, &owner).await, 1);
    assert_eq!(job_status(&store, "project-1").await, "failed");
}

#[tokio::test]
async fn fcm_app_id_mismatch_is_refused_before_any_token_fetch_or_request() {
    let sender = FakeFcmSender::default();
    let tokens = CountingTokens::new();
    let store = store_with_fcm(sender.clone(), Arc::clone(&tokens)).await;
    let owner = owner();
    let phone = AndroidDevice::new("fcm-token-phone");
    let node = android_node(&store, &owner, "com.example.other", &[("phone", &phone)]).await;

    publish(
        &store,
        &owner,
        &node,
        notification_item("foreign-1", "dm", 1),
    )
    .await;

    assert!(
        sender.calls().is_empty(),
        "no FCM request for a foreign app"
    );
    assert_eq!(tokens.fetches.load(Ordering::SeqCst), 0);
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![("phone".to_string(), FCM_APP_ID_MISMATCH.to_string())]
    );
    assert_eq!(
        device_status(&store, &node, "phone").await,
        DEVICE_STATUS_ACTIVE
    );
    assert_eq!(job_status(&store, "foreign-1").await, "failed");
}

#[tokio::test]
async fn fcm_rejected_access_token_is_refreshed_and_retried_once() {
    let phone = AndroidDevice::new("fcm-token-phone");
    let sender = FakeFcmSender::with_outcomes(vec![(
        "fcm-token-phone",
        vec![
            FcmOutcome::Unauthenticated {
                detail: FcmErrorDetail::default(),
            },
            FcmOutcome::Sent,
        ],
    )]);
    let tokens = CountingTokens::new();
    let store = store_with_fcm(sender.clone(), Arc::clone(&tokens)).await;
    let owner = owner();
    let node = android_node(&store, &owner, APP_ID, &[("phone", &phone)]).await;

    publish(
        &store,
        &owner,
        &node,
        notification_item("refresh-1", "dm", 1),
    )
    .await;

    let calls = sender.calls_for(&phone.token);
    assert_eq!(calls.len(), 2, "one retry with a fresh token");
    assert_eq!(calls[0].access_token, "ya29.token-1");
    assert_eq!(calls[1].access_token, "ya29.token-2");
    assert_eq!(tokens.invalidations.load(Ordering::SeqCst), 1);
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![("phone".to_string(), FCM_DELIVERED.to_string())]
    );
}

#[tokio::test]
async fn fcm_access_token_rejected_twice_keeps_the_device_and_fails_the_job() {
    let phone = AndroidDevice::new("fcm-token-phone");
    let sender = FakeFcmSender::with_outcomes(vec![(
        "fcm-token-phone",
        vec![FcmOutcome::Unauthenticated {
            detail: FcmErrorDetail::default(),
        }],
    )]);
    let store = store_with_fcm(sender.clone(), CountingTokens::new()).await;
    let owner = owner();
    let node = android_node(&store, &owner, APP_ID, &[("phone", &phone)]).await;

    publish(&store, &owner, &node, notification_item("auth-1", "dm", 1)).await;

    assert_eq!(
        sender.calls_for(&phone.token).len(),
        2,
        "retried exactly once"
    );
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![("phone".to_string(), FCM_PROVIDER_AUTH.to_string())]
    );
    assert_eq!(
        device_status(&store, &node, "phone").await,
        DEVICE_STATUS_ACTIVE,
        "a credentials problem is not the device's fault"
    );
    assert_eq!(job_status(&store, "auth-1").await, "failed");
}

#[tokio::test]
async fn refused_oauth_grant_is_provider_auth_without_contacting_fcm() {
    let phone = AndroidDevice::new("fcm-token-phone");
    let sender = FakeFcmSender::default();
    let tokens = CountingTokens::failing_with(Some(FcmTokenError::Rejected {
        status: 400,
        error: OAuthErrorCode::InvalidGrant,
    }));
    let store = store_with_fcm(sender.clone(), tokens).await;
    let owner = owner();
    let node = android_node(&store, &owner, APP_ID, &[("phone", &phone)]).await;

    publish(&store, &owner, &node, notification_item("grant-1", "dm", 1)).await;

    assert!(sender.calls().is_empty());
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![("phone".to_string(), FCM_PROVIDER_AUTH.to_string())]
    );
    assert_eq!(
        device_status(&store, &node, "phone").await,
        DEVICE_STATUS_ACTIVE
    );
    assert_eq!(job_status(&store, "grant-1").await, "failed");
}

#[tokio::test]
async fn token_endpoint_outage_requeues_without_an_uncertain_send() {
    let phone = AndroidDevice::new("fcm-token-phone");
    let sender = FakeFcmSender::default();
    let tokens = CountingTokens::failing_with(Some(FcmTokenError::Transient(
        TransientFailure::ServerError { status: 503 },
    )));
    let store = store_with_fcm(sender.clone(), tokens).await;
    let owner = owner();
    let node = android_node(&store, &owner, APP_ID, &[("phone", &phone)]).await;

    publish(&store, &owner, &node, notification_item("oauth-1", "dm", 1)).await;

    assert!(sender.calls().is_empty(), "FCM was never contacted");
    assert_eq!(
        attempt_statuses(&store, &node).await,
        vec![("phone".to_string(), FCM_TOKEN_TRANSIENT.to_string())]
    );
    assert_eq!(job_status(&store, "oauth-1").await, "queued");
    assert_eq!(
        job_uncertain_send(&store, "oauth-1").await,
        0,
        "a token-endpoint failure cannot have delivered anything"
    );
}

#[tokio::test]
async fn fcm_transient_failure_requeues_without_resending_to_delivered_siblings() {
    let flaky = AndroidDevice::new("fcm-token-flaky");
    let good = AndroidDevice::new("fcm-token-good");
    let sender = FakeFcmSender::with_outcomes(vec![(
        "fcm-token-flaky",
        vec![
            FcmOutcome::Transient {
                cause: FcmTransient::Failure(TransientFailure::ServerError { status: 503 }),
                retry_after: Some(Duration::from_secs(120)),
            },
            FcmOutcome::Sent,
        ],
    )]);
    let store = store_with_fcm(sender.clone(), CountingTokens::new()).await;
    let owner = owner();
    let node = android_node(
        &store,
        &owner,
        APP_ID,
        &[("flaky-phone", &flaky), ("good-phone", &good)],
    )
    .await;

    publish(&store, &owner, &node, notification_item("retry-1", "dm", 1)).await;

    assert_eq!(job_status(&store, "retry-1").await, "queued");
    assert_eq!(
        job_uncertain_send(&store, "retry-1").await,
        1,
        "FCM may have accepted a message that timed out or got a 5xx"
    );
    assert_eq!(
        device_status(&store, &node, "flaky-phone").await,
        DEVICE_STATUS_ACTIVE
    );
    assert!(attempt_statuses(&store, &node)
        .await
        .contains(&("flaky-phone".to_string(), FCM_TRANSIENT.to_string())));

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
    assert_eq!(sender.calls_for(&flaky.token).len(), 2);
    assert_eq!(
        sender.calls_for(&good.token).len(),
        1,
        "#1123: an already-delivered Android device is not pushed twice"
    );
}
