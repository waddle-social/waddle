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

/// Mirrors `crate::push_service::dispatch::ATTEMPT_STATUS_FAKE_SENT_NON_WEB`
/// (recorded for the stubbed FCM platform until #530).
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

#[tokio::test]
async fn publish_notification_fans_out_to_active_devices_only() {
    // Queue-mechanics test: uses the FCM platform so no real provider
    // needs wiring. FCM records the `fake-sent` stub until #530 lands
    // its sender (APNs is dispatched for real since #529).
    let store = store().await;
    let owner = owner();
    let node = store.ensure_node(&owner, "web").await.expect("push node");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Fcm, "test")
                .with_provider_endpoint(Some("https://push.example.com/one".to_string())),
        )
        .await
        .expect("device one");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("dev-2", node.node(), PushDevicePlatform::Fcm, "test")
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
                PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Fcm, "test"),
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
async fn uncertain_publish_job_survives_reopen_and_retries_after_lease_recovery() {
    // Queue-mechanics: uses FCM so the fake-sent stub applies; Web
    // and APNs require a wired provider.
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("push-service-publish-jobs.sqlite3");
    let owner = owner();
    let node_id;
    let acceptance_id;
    let lease_before;
    {
        let db = open_local("push-service-jobs", &path).await;
        let store = store_on(db).await;
        let node = store.ensure_node(&owner, "web").await.expect("node");
        node_id = node.node().to_string();
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Fcm, "test"),
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
    assert!(
        reopened
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("recover expired lease")
            .is_empty(),
        "lease recovery schedules retry backoff before another provider attempt"
    );
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
    let node = store.ensure_node(&owner, "web").await.expect("node");
    store
        .upsert_device(
            &owner,
            PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Fcm, "test"),
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
            PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Fcm, "test"),
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
