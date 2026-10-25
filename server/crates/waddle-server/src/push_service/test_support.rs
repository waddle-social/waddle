//! Shared helpers for the Push Service inline test modules.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jid::BareJid;
use minidom::Element;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::push::apns::{
    ApnsOutcome, ApnsProviderTokenSource, ApnsRequest, ApnsSender, ApnsTopic,
    CachingApnsTokenSigner, SystemApnsClock,
};
use waddle_xmpp::push::fcm::{
    FcmAccessToken, FcmAccessTokenSource, FcmAppId, FcmOutcome, FcmProjectId, FcmRequest,
    FcmSender, FcmTokenError,
};
use waddle_xmpp::xep::xep0357::NS_PUSH;
use waddle_xmpp::XmppError;

use crate::db::{Database, IntoParams};

use super::store::DatabasePushServiceStore;
use super::types::{PushDevicePlatform, PushDeviceRegistration};

/// The Android application id the fake FCM provider serves. Nodes that
/// carry FCM devices in worker fixtures are created with this app id.
pub(crate) const FCM_TEST_APP_ID: &str = "social.waddle.android";

pub(super) async fn store() -> DatabasePushServiceStore {
    DatabasePushServiceStore::try_new(
        Database::in_memory("push-service")
            .await
            .expect("push service db"),
    )
    .await
    .expect("push service store")
}

/// The bundle id the fake APNs provider serves.
pub(crate) const APNS_TEST_TOPIC: &str = "social.waddle.ios";

/// A store with a fake FCM provider whose sender accepts every message.
pub(super) async fn store_with_fake_fcm() -> (DatabasePushServiceStore, FakeFcmSender) {
    let sender = FakeFcmSender::default();
    let store = with_fake_fcm(store().await, sender.clone());
    (store, sender)
}

/// Install a fake FCM provider serving [`FCM_TEST_APP_ID`].
pub(crate) fn with_fake_fcm(
    store: DatabasePushServiceStore,
    sender: FakeFcmSender,
) -> DatabasePushServiceStore {
    store.with_fcm_provider(
        Arc::new(StaticFcmTokens),
        Arc::new(sender),
        FcmProjectId::parse("waddle-test").expect("project id"),
        FcmAppId::parse(FCM_TEST_APP_ID).expect("app id"),
    )
}

/// Install a fake APNs provider serving [`APNS_TEST_TOPIC`] whose sender
/// accepts every notification.
pub(crate) fn with_fake_apns(store: DatabasePushServiceStore) -> DatabasePushServiceStore {
    use p256::pkcs8::EncodePrivateKey;
    let pem = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng)
        .to_pkcs8_pem(Default::default())
        .expect("p8 key");
    let tokens: Arc<dyn ApnsProviderTokenSource> = Arc::new(
        CachingApnsTokenSigner::from_pkcs8_pem(
            waddle_xmpp::push::apns::ApnsTeamId::parse("TEAM123456").expect("team id"),
            waddle_xmpp::push::apns::ApnsKeyId::parse("KEY1234567").expect("key id"),
            &pem,
            Arc::new(SystemApnsClock),
        )
        .expect("APNs signer"),
    );
    store.with_apns_provider(
        tokens,
        Arc::new(AcceptingApnsSender),
        ApnsTopic::parse(APNS_TEST_TOPIC).expect("topic"),
    )
}

/// A deliverable APNs device registration.
pub(crate) fn apns_registration(device_id: &str, node: &str) -> PushDeviceRegistration {
    PushDeviceRegistration::new(device_id, node, PushDevicePlatform::Apns, "prod")
        .with_provider_token(Some("0a1b2c3d4e5f60718293a4b5c6d7e8f9".to_string()))
}

struct AcceptingApnsSender;

impl ApnsSender for AcceptingApnsSender {
    fn send(
        &self,
        _request: ApnsRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = ApnsOutcome> + Send + '_>> {
        Box::pin(async { ApnsOutcome::Sent { apns_id: None } })
    }
}

pub(super) fn owner() -> BareJid {
    "alice@example.com".parse().expect("owner jid")
}

/// XEP-0357 notification carrying the Waddle routing context the chat
/// publisher attaches, so a provider-backed worker can parse it.
pub(super) fn notification_item(item_id: &str) -> PubSubItem {
    let context = Element::builder("context", "urn:waddle:push:context:0")
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            "bob@example.com",
        )
        .attr(minidom::rxml::xml_ncname!("class").to_owned(), "dm")
        .build();
    PubSubItem::new(
        Some(item_id.to_string()),
        Some(
            Element::builder("notification", NS_PUSH)
                .append(context)
                .build(),
        ),
    )
}

/// An Android client's RFC 8291 envelope-encryption keys.
pub(crate) struct FcmEnvelopeKeys {
    pub(crate) p256dh: String,
    pub(crate) auth: String,
}

impl FcmEnvelopeKeys {
    pub(crate) fn generate() -> Self {
        let secret = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
        let auth: [u8; 16] = rand::random();
        Self {
            p256dh: URL_SAFE_NO_PAD.encode(secret.public_key().to_encoded_point(false).as_bytes()),
            auth: URL_SAFE_NO_PAD.encode(auth),
        }
    }
}

/// A deliverable FCM device registration: token plus envelope keys.
pub(crate) fn fcm_registration(device_id: &str, node: &str) -> PushDeviceRegistration {
    let keys = FcmEnvelopeKeys::generate();
    PushDeviceRegistration::new(device_id, node, PushDevicePlatform::Fcm, "prod")
        .with_provider_token(Some(format!("fcm-token-{device_id}")))
        .with_provider_key_material(Some(keys.p256dh))
        .with_provider_auth(Some(keys.auth))
}

/// Access-token source that always returns the same token.
pub(super) struct StaticFcmTokens;

impl FcmAccessTokenSource for StaticFcmTokens {
    fn current(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<FcmAccessToken, FcmTokenError>> + Send + '_>> {
        Box::pin(async { Ok(FcmAccessToken::new("ya29.test".to_string())) })
    }

    fn invalidate(&self, _rejected: &FcmAccessToken) {}
}

/// FCM sender that accepts every message and counts the requests.
#[derive(Clone, Default)]
pub(crate) struct FakeFcmSender {
    sent: Arc<Mutex<usize>>,
}

impl FakeFcmSender {
    pub(super) fn sent(&self) -> usize {
        *self.sent.lock().expect("fake FCM sender lock")
    }
}

impl FcmSender for FakeFcmSender {
    fn send(
        &self,
        _request: FcmRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = FcmOutcome> + Send + '_>> {
        *self.sent.lock().expect("fake FCM sender lock") += 1;
        Box::pin(async { FcmOutcome::Sent })
    }
}

pub(super) fn publish_options_with_field(var: &str, value: &str) -> Element {
    Element::builder("x", waddle_xmpp::xep::NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .append(
                    Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                        .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
                .append(
                    Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                        .append(value)
                        .build(),
                )
                .build(),
        )
        .build()
}

pub(super) async fn scalar_i64(
    store: &DatabasePushServiceStore,
    sql: &str,
    params: impl IntoParams,
) -> i64 {
    let mut rows = store.query(sql, params).await.expect("scalar query");
    let row = rows.next().await.expect("scalar row").expect("scalar row");
    row.get(0).expect("scalar value")
}

pub(super) fn assert_item_not_found(error: XmppError) {
    assert!(matches!(
        error,
        XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
            ..
        }
    ));
}
