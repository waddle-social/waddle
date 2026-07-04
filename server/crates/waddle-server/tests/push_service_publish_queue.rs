//! Push Service publish fan-out and publish-job queue durability tests
//! driven exclusively through the store's public API: fan-out targets
//! only active devices, payload validation, delivery-attempt/job
//! persistence across store reopen, and retry wake-up semantics.

use std::path::Path;

use jid::BareJid;
use minidom::Element;
use tempfile::tempdir;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, IntoParams, Rows};
use waddle_server::db_params;
use waddle_server::push_service::{
    DatabasePushServiceStore, PushDevicePlatform, PushDeviceRegistration,
};
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::xep::xep0357::NS_PUSH;
use waddle_xmpp::XmppError;

/// Mirrors `crate::push_service::dispatch::ATTEMPT_STATUS_FAKE_SENT_NON_WEB`.
const ATTEMPT_STATUS_FAKE_SENT_NON_WEB: &str = "fake-sent";

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

fn notification_item(item_id: &str) -> PubSubItem {
    PubSubItem::new(
        Some(item_id.to_string()),
        Some(Element::builder("notification", NS_PUSH).build()),
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

async fn scalar_optional_i64(
    store: &DatabasePushServiceStore,
    sql: &str,
    params: impl IntoParams,
) -> Option<i64> {
    let mut rows = query(store, sql, params).await;
    let row = rows.next().await.expect("scalar row").expect("scalar row");
    row.get(0).expect("scalar optional value")
}

#[tokio::test]
async fn publish_notification_fans_out_to_active_devices_only() {
    // Queue-mechanics test: uses Apns platform so we don't need a
    // Web Push provider wired. APNS sender lands in #529; until
    // then non-Web platforms record the legacy `fake-sent` marker.
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Apns, "test")
                .with_provider_endpoint(Some("https://push.example.com/one".to_string())),
        )
        .await
        .expect("device one");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("dev-2", node.node(), PushDevicePlatform::Apns, "test")
                .with_provider_endpoint(Some("https://push.example.com/two".to_string())),
        )
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
    assert_eq!(attempts[0].status(), ATTEMPT_STATUS_FAKE_SENT_NON_WEB);
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
        let node = store.ensure_node(&owner, "web").await.expect("node");
        node_id = node.node().to_string();
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Apns, "test"),
            )
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
    assert_eq!(attempts[0].status(), ATTEMPT_STATUS_FAKE_SENT_NON_WEB);
}

#[tokio::test]
async fn queued_publish_job_survives_reopen_and_retries_after_dispatch_failure() {
    // Queue-mechanics: uses Apns so the fake-sent path applies; the
    // Web platform now requires a wired provider.
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("push-service-publish-jobs.sqlite3");
    let owner = owner();
    let node_id;
    {
        let db = open_local("push-service-jobs", &path).await;
        let store = store_on(db).await;
        let node = store.ensure_node(&owner, "web").await.expect("node");
        node_id = node.node().to_string();
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Apns, "test"),
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
                &notification_item("retry-after-failure"),
                &owner,
            )
            .await
            .expect_err("forced dispatch failure keeps job queued");
        execute(&store, "DROP TRIGGER fail_push_delivery_attempt_insert", ()).await;
        let queued = store.queued_publish_jobs().await.expect("queued jobs");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id(), "retry-after-failure");
        assert!(attempts.is_empty());
    }

    let reopened_db = open_local("push-service-jobs-reopen", &path).await;
    let reopened = store_on(reopened_db).await;
    execute(
        &reopened,
        "UPDATE push_publish_jobs SET next_retry_at_ms = NULL WHERE item_id = ?",
        db_params!["retry-after-failure"],
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
        .expect_err("forced dispatch failure keeps job queued");
    execute(&store, "DROP TRIGGER fail_push_delivery_attempt_insert", ()).await;
    let retry_before = scalar_optional_i64(
        &store,
        "SELECT next_retry_at_ms FROM push_publish_jobs WHERE item_id = ?",
        db_params!["retry-after-transient-failure"],
    )
    .await
    .expect("transient retry deadline");

    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
        )
        .await
        .expect("device refresh");
    let retry_after = scalar_optional_i64(
        &store,
        "SELECT next_retry_at_ms FROM push_publish_jobs WHERE item_id = ?",
        db_params!["retry-after-transient-failure"],
    )
    .await
    .expect("transient retry deadline after device refresh");

    assert_eq!(retry_after, retry_before);
}

#[tokio::test]
async fn zero_device_publish_job_remains_retryable_until_device_returns() {
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("node");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Apns, "test"),
        )
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
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Apns, "test"),
        )
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
    assert!(queued_after.is_empty());
}
