//! Push Service publish fan-out and publish-job queue durability tests
//! driven exclusively through the store's public API: fan-out targets
//! only active devices, payload validation, delivery-attempt/job
//! persistence across store reopen, and retry wake-up semantics.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jid::BareJid;
use minidom::Element;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use tempfile::tempdir;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, IntoParams, Rows};
use waddle_server::db_params;
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::push::fcm::{
    FcmAccessToken, FcmAccessTokenSource, FcmAppId, FcmOutcome, FcmProjectId, FcmRequest,
    FcmSender, FcmTokenError,
};
use waddle_xmpp::xep::xep0357::NS_PUSH;
use waddle_xmpp::XmppError;

/// Mirrors `fcm_dispatch::ATTEMPT_STATUS_FCM_DELIVERED`.
const ATTEMPT_STATUS_FCM_DELIVERED: &str = "fcm-delivered";
/// Android application id the fake FCM provider serves.
const FCM_APP_ID: &str = "social.waddle.android";

/// FCM transport that accepts every message: these are queue-mechanics
/// tests, so the provider only has to answer.
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

async fn store() -> DatabasePushServiceStore {
    store_on(
        Database::in_memory("push-service")
            .await
            .expect("push service db"),
    )
    .await
}

async fn store_on(db: Database) -> DatabasePushServiceStore {
    DatabasePushServiceStore::new_with_secret_key(db, b"waddle-push-service-test-secret-key")
        .await
        .expect("push service store")
        .with_fcm_provider(
            Arc::new(StaticFcmTokens),
            Arc::new(AcceptingFcmSender),
            FcmProjectId::parse("waddle-test").expect("project id"),
            FcmAppId::parse(FCM_APP_ID).expect("app id"),
        )
}

/// A deliverable FCM device: registration token plus the client's
/// RFC 8291 envelope keys.
fn fcm_device(device_id: &str, node: &str) -> PushDeviceRegistration {
    let secret = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
    let auth: [u8; 16] = rand::random();
    PushDeviceRegistration::new(device_id, node, PushDevicePlatform::Fcm, "prod")
        .with_provider_token(Some(format!("fcm-token-{device_id}")))
        .with_provider_key_material(Some(
            URL_SAFE_NO_PAD.encode(secret.public_key().to_encoded_point(false).as_bytes()),
        ))
        .with_provider_auth(Some(URL_SAFE_NO_PAD.encode(auth)))
}

/// Open a SQLite database at a local file path. Mirrors the crate's
/// test-only `Database::open_local` constructor, which is not exported
/// to integration tests.
async fn open_local(name: &str, path: &Path) -> Database {
    let database_url = format!("sqlite://{}", path.to_string_lossy());
    Database::from_config(
        name,
        &DatabaseConfig::new(DatabaseDriver::Sqlite, database_url),
    )
    .await
    .expect("open local database")
}

fn owner() -> BareJid {
    "alice@example.com".parse().expect("owner jid")
}

/// XEP-0357 notification with the Waddle routing context the chat
/// publisher attaches, so the provider-backed worker can parse it.
fn notification_item(item_id: &str) -> PubSubItem {
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

async fn execute(store: &DatabasePushServiceStore, sql: &str, params: impl IntoParams) {
    let db = store.database();
    let conn = db.guard().await.expect("db guard");
    conn.execute(sql, params).await.expect("execute");
}

async fn query(store: &DatabasePushServiceStore, sql: &str, params: impl IntoParams) -> Rows {
    let db = store.database();
    let conn = db.guard().await.expect("db guard");
    conn.query(sql, params).await.expect("query")
}

#[tokio::test]
async fn publish_notification_fans_out_to_active_devices_only() {
    // Queue-mechanics test: FCM devices behind a fake FCM transport.
    let store = store().await;
    let owner = owner();
    let node = store
        .ensure_node(&owner, FCM_APP_ID)
        .await
        .expect("push node");
    store
        .upsert_device(&owner, fcm_device("dev-1", node.node()))
        .await
        .expect("device one");
    store
        .upsert_device(&owner, fcm_device("dev-2", node.node()))
        .await
        .expect("device two");
    store
        .disable_device_for_owner(&owner, node.node(), "dev-2", Some("expired"))
        .await
        .expect("disable device");

    let result = store
        .publish_notification_from_user_server(node.node(), &notification_item("push-1"), &owner)
        .await
        .expect("publish");

    assert_eq!(result.item_id(), "push-1");
    assert_eq!(result.attempted_devices(), 1);
    let attempts = store
        .delivery_attempts_for_node(node.node())
        .await
        .expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].device_id(), "dev-1");
    assert_eq!(attempts[0].status(), ATTEMPT_STATUS_FCM_DELIVERED);
}

#[tokio::test]
async fn publish_notification_requires_xep0357_payload() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    let item = PubSubItem::new(
        Some("bad".to_string()),
        Some(Element::builder("x", "urn:waddle:test").build()),
    );

    let err = store
        .publish_notification_from_user_server(node.node(), &item, &owner)
        .await
        .expect_err("reject wrong payload");
    // XEP-0060 §7.1.3.4: malformed payload must surface as the
    // typed `<invalid-payload xmlns='http://jabber.org/protocol/
    // pubsub#errors'/>` extension condition. The dispatch layer
    // maps this onto `<bad-request/>` + the extension element on
    // the wire, but the typed error is the carrier inside the
    // process.
    assert!(
        matches!(err, XmppError::PubSubInvalidPayload(_)),
        "expected pubsub:invalid-payload, got {err:?}"
    );
}

#[tokio::test]
async fn push_delivery_attempts_survive_store_reopen() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("push-service-attempts.sqlite3");
    let owner = owner();
    let node_id;
    {
        let db = open_local("push-service-attempts", &path).await;
        let store = store_on(db).await;
        let node = store.ensure_node(&owner, FCM_APP_ID).await.expect("node");
        node_id = node.node().to_string();
        store
            .upsert_device(&owner, fcm_device("dev-1", node.node()))
            .await
            .expect("device");
        store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("durable-attempt"),
                &owner,
            )
            .await
            .expect("publish");
    }

    let reopened_db = open_local("push-service-attempts-reopen", &path).await;
    let reopened = store_on(reopened_db).await;
    let attempts = reopened
        .delivery_attempts_for_node(&node_id)
        .await
        .expect("attempts");

    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].device_id(), "dev-1");
    assert_eq!(attempts[0].item_id(), "durable-attempt");
    assert_eq!(attempts[0].status(), ATTEMPT_STATUS_FCM_DELIVERED);
}

#[tokio::test]
async fn uncertain_publish_job_survives_reopen_and_retries_after_lease_recovery() {
    // Queue-mechanics: FCM devices behind a fake FCM transport.
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("push-service-publish-jobs.sqlite3");
    let owner = owner();
    let node_id;
    let acceptance_id;
    let lease_before;
    {
        let db = open_local("push-service-jobs", &path).await;
        let store = store_on(db).await;
        let node = store.ensure_node(&owner, FCM_APP_ID).await.expect("node");
        node_id = node.node().to_string();
        store
            .upsert_device(&owner, fcm_device("dev-1", node.node()))
            .await
            .expect("device");
        execute(
            &store,
            r#"
            CREATE TRIGGER fail_push_delivery_attempt_insert
            BEFORE INSERT ON push_delivery_attempts
            BEGIN
                SELECT RAISE(ABORT, 'forced push delivery attempt failure');
            END
            "#,
            (),
        )
        .await;

        store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("retry-after-failure"),
                &owner,
            )
            .await
            .expect_err("phase3 failure retains an uncertain in-progress lease");
        execute(&store, "DROP TRIGGER fail_push_delivery_attempt_insert", ()).await;
        let queued = store.queued_publish_jobs().await.expect("queued jobs");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        assert!(
            queued.is_empty(),
            "unproved sends retain ownership until their lease expires"
        );
        assert!(attempts.is_empty());
        let mut rows = query(&store,
            "SELECT job_id, status, uncertain_send, claim_token, claimed_at_ms, next_retry_at_ms FROM push_publish_jobs WHERE item_id = ?",
            db_params!["retry-after-failure"]).await;
        let row = rows.next().await.expect("row").expect("owned acceptance");
        acceptance_id = row.get::<String>(0).expect("acceptance identity");
        assert_eq!(row.get::<String>(1).expect("status"), "in-progress");
        assert_eq!(row.get::<i64>(2).expect("uncertainty"), 1);
        lease_before = (
            row.get::<Option<String>>(3).expect("claim token"),
            row.get::<Option<i64>>(4).expect("claim time"),
            row.get::<Option<i64>>(5).expect("nullable retry deadline"),
        );
        assert!(lease_before.0.is_some());
        assert!(lease_before.1.is_some());
        assert_eq!(lease_before.2, None);
    }

    let reopened_db = open_local("push-service-jobs-reopen", &path).await;
    let reopened = store_on(reopened_db).await;
    let mut rows = query(&reopened,
        "SELECT status, uncertain_send, claim_token, claimed_at_ms, next_retry_at_ms FROM push_publish_jobs WHERE job_id = ?",
        db_params![acceptance_id.clone()]).await;
    let row = rows
        .next()
        .await
        .expect("row")
        .expect("reopened acceptance");
    assert_eq!(row.get::<String>(0).expect("status"), "in-progress");
    assert_eq!(row.get::<i64>(1).expect("uncertainty"), 1);
    assert_eq!(
        (
            row.get::<Option<String>>(2).expect("claim token"),
            row.get::<Option<i64>>(3).expect("claim time"),
            row.get::<Option<i64>>(4).expect("retry deadline")
        ),
        lease_before
    );
    execute(
        &reopened,
        "UPDATE push_publish_jobs SET claimed_at_ms = 1 WHERE job_id = ?",
        db_params![acceptance_id.clone()],
    )
    .await;
    let recovery_started = chrono::Utc::now().timestamp_millis();
    assert!(
        reopened
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("recover expired lease")
            .is_empty(),
        "lease recovery schedules retry backoff before another provider attempt"
    );
    let recovery_finished = chrono::Utc::now().timestamp_millis();
    let mut recovery_rows = query(
        &reopened,
        "SELECT attempt_count, next_retry_at_ms FROM push_publish_jobs WHERE job_id = ?",
        db_params![acceptance_id.clone()],
    )
    .await;
    let recovery_row = recovery_rows
        .next()
        .await
        .expect("row")
        .expect("recovered lease");
    assert_eq!(
        recovery_row
            .get::<i64>(0)
            .expect("completed failed attempts"),
        1
    );
    let retry_at = recovery_row.get::<i64>(1).expect("persisted backoff");
    assert!((recovery_started + 45_000..=recovery_finished + 75_000).contains(&retry_at));
    drop(recovery_rows);
    let recovered = reopened
        .queued_publish_jobs()
        .await
        .expect("recovered queue");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].job_id(), acceptance_id);
    assert_eq!(recovered[0].item_id(), "retry-after-failure");
    execute(
        &reopened,
        "UPDATE push_publish_jobs SET next_retry_at_ms = NULL WHERE job_id = ?",
        db_params![acceptance_id.clone()],
    )
    .await;
    let results = reopened
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("drain queued publish job");
    let attempts = reopened
        .delivery_attempts_for_node(&node_id)
        .await
        .expect("attempts after retry");
    let queued = reopened.queued_publish_jobs().await.expect("queued jobs");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].item_id(), "retry-after-failure");
    assert_eq!(results[0].attempted_devices(), 1);
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].item_id(), "retry-after-failure");
    assert_eq!(attempts[0].status(), ATTEMPT_STATUS_FCM_DELIVERED);
    assert!(queued.is_empty());
}

#[tokio::test]
async fn device_registration_wakes_only_no_device_retry_jobs() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
        )
        .await
        .expect("device");
    execute(
        &store,
        r#"
        CREATE TRIGGER fail_push_delivery_attempt_insert
        BEFORE INSERT ON push_delivery_attempts
        BEGIN
            SELECT RAISE(ABORT, 'forced push delivery attempt failure');
        END
        "#,
        (),
    )
    .await;
    store
        .publish_notification_from_user_server(
            node.node(),
            &notification_item("retry-after-transient-failure"),
            &owner,
        )
        .await
        .expect_err("phase3 failure preserves the uncertain active lease");
    execute(&store, "DROP TRIGGER fail_push_delivery_attempt_insert", ()).await;
    let mut rows = query(&store,
        "SELECT status, uncertain_send, claim_token, claimed_at_ms, next_retry_at_ms FROM push_publish_jobs WHERE item_id = ?",
        db_params!["retry-after-transient-failure"]).await;
    let row = rows.next().await.expect("row").expect("uncertain lease");
    let before = (
        row.get::<String>(0).expect("status"),
        row.get::<i64>(1).expect("uncertainty"),
        row.get::<Option<String>>(2).expect("claim token"),
        row.get::<Option<i64>>(3).expect("claim time"),
        row.get::<Option<i64>>(4).expect("nullable retry deadline"),
    );
    assert_eq!(before.0, "in-progress");
    assert_eq!(before.1, 1);
    assert!(before.2.is_some());
    assert!(before.3.is_some());
    assert_eq!(before.4, None);

    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
        )
        .await
        .expect("device refresh");
    let mut rows = query(&store,
        "SELECT status, uncertain_send, claim_token, claimed_at_ms, next_retry_at_ms FROM push_publish_jobs WHERE item_id = ?",
        db_params!["retry-after-transient-failure"]).await;
    let row = rows
        .next()
        .await
        .expect("row")
        .expect("lease after device refresh");
    let after = (
        row.get::<String>(0).expect("status"),
        row.get::<i64>(1).expect("uncertainty"),
        row.get::<Option<String>>(2).expect("claim token"),
        row.get::<Option<i64>>(3).expect("claim time"),
        row.get::<Option<i64>>(4).expect("nullable retry deadline"),
    );
    assert_eq!(
        after, before,
        "device refresh must not wake or alter an owned uncertain send"
    );
}

#[tokio::test]
async fn zero_device_publish_job_remains_retryable_until_device_returns() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, FCM_APP_ID).await.expect("node");
    store
        .upsert_device(&owner, fcm_device("dev-1", node.node()))
        .await
        .expect("device");
    store
        .disable_device_for_owner(&owner, node.node(), "dev-1", None)
        .await
        .expect("disable device");

    let result = store
        .publish_notification_from_user_server(
            node.node(),
            &notification_item("retry-when-device-returns"),
            &owner,
        )
        .await
        .expect("publish with no active devices stays queued");
    let queued = store.queued_publish_jobs().await.expect("queued jobs");

    assert_eq!(result.attempted_devices(), 0);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].item_id(), "retry-when-device-returns");

    store
        .upsert_device(&owner, fcm_device("dev-1", node.node()))
        .await
        .expect("reenable device");
    let retried = store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("drain queued");
    let attempts = store
        .delivery_attempts_for_node(node.node())
        .await
        .expect("attempts");
    let queued_after = store.queued_publish_jobs().await.expect("queued jobs");

    assert_eq!(retried.len(), 1);
    assert_eq!(retried[0].attempted_devices(), 1);
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].item_id(), "retry-when-device-returns");
    assert_eq!(attempts[0].status(), ATTEMPT_STATUS_FCM_DELIVERED);
    assert!(queued_after.is_empty());
}
