use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp::pubsub::{
    PubSubItem, PubSubStorage, PublicationError, PublicationNode, PublicationVersion,
    VersionedPublishResult,
};

use super::DatabasePubSubStorage;

fn item(body: &str, id: &str) -> PubSubItem {
    PubSubItem {
        id: Some(id.to_owned()),
        publisher: None,
        payload: Some(
            minidom::Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH)
                .append(
                    minidom::Element::builder("value", "urn:test:push-storage")
                        .append(body)
                        .build(),
                )
                .build(),
        ),
    }
}

async fn exercise_ordering(storage: Arc<DatabasePubSubStorage>) {
    let service: BareJid = "push@example.com".parse().expect("service");
    let publisher: BareJid = "alice@example.com".parse().expect("publisher");
    let node = PublicationNode::new(format!("ordering-{}", uuid::Uuid::new_v4())).expect("node");
    storage
        .get_or_create_node(&service, node.as_str())
        .await
        .expect("node");
    let version = PublicationVersion::new(2, uuid::Uuid::new_v4()).expect("version");
    let latest = item("latest", "same");
    // Simulate an earlier worker paused outside the PubSub database, then a
    // newer wire publish commits before the older worker finally enters it.
    let (release, delayed) = tokio::sync::oneshot::channel();
    let older_storage = storage.clone();
    let older_service = service.clone();
    let older_publisher = publisher.clone();
    let older_node = node.clone();
    let older = tokio::spawn(async move {
        delayed.await.expect("release older worker");
        older_storage
            .publish_push_item_versioned(
                &older_service,
                &older_publisher,
                &older_node,
                &item("old", "different"),
                PublicationVersion::new(1, uuid::Uuid::new_v4()).expect("version"),
            )
            .await
    });
    storage
        .publish_push_item_versioned(&service, &publisher, &node, &latest, version)
        .await
        .expect("commit newer and lose reply");
    release.send(()).expect("release older");
    assert!(matches!(
        older.await.expect("older task").expect("older result"),
        VersionedPublishResult::Superseded
    ));
    let before = storage
        .get_items(&service, node.as_str(), None, &[])
        .await
        .expect("items");
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].to_pubsub_item().payload, latest.payload);
    assert!(matches!(
        storage
            .publish_push_item_versioned(&service, &publisher, &node, &latest, version)
            .await
            .expect("lost reply replay"),
        VersionedPublishResult::AlreadyApplied
    ));
    let after = storage
        .get_items(&service, node.as_str(), None, &[])
        .await
        .expect("items");
    assert_eq!(before[0].published_at, after[0].published_at);
    assert!(matches!(
        storage
            .publish_push_item_versioned(
                &service,
                &publisher,
                &node,
                &item("mutated", "same"),
                version
            )
            .await,
        Err(PublicationError::IntegrityConflict)
    ));
    assert!(matches!(
        storage
            .publish_push_item_versioned(
                &service,
                &publisher,
                &node,
                &latest,
                PublicationVersion::new(2, uuid::Uuid::new_v4()).expect("version")
            )
            .await,
        Err(PublicationError::IntegrityConflict)
    ));
    storage
        .retract_item(&service, node.as_str(), "same")
        .await
        .expect("retract");
    assert!(matches!(
        storage
            .publish_push_item_versioned(&service, &publisher, &node, &latest, version)
            .await
            .expect("retracted replay"),
        VersionedPublishResult::AlreadyApplied
    ));
    assert!(storage
        .get_items(&service, node.as_str(), None, &[])
        .await
        .expect("items")
        .is_empty());
    assert!(matches!(
        storage
            .publish_push_item_versioned(
                &service,
                &publisher,
                &node,
                &item("changed after retract", "same"),
                version
            )
            .await,
        Err(PublicationError::IntegrityConflict)
    ));
    assert_digest_only_metadata(&storage, &service, &node).await;
    storage
        .delete_node(&service, node.as_str())
        .await
        .expect("delete node");
    storage
        .get_or_create_node(&service, node.as_str())
        .await
        .expect("reuse node");
    assert!(matches!(
        storage
            .publish_push_item_versioned(&service, &publisher, &node, &latest, version)
            .await
            .expect("reused node replay"),
        VersionedPublishResult::AlreadyApplied
    ));
    assert_digest_only_metadata(&storage, &service, &node).await;
    assert!(matches!(
        storage
            .publish_push_item_versioned(
                &service,
                &publisher,
                &node,
                &item("changed after node reuse", "same"),
                version
            )
            .await,
        Err(PublicationError::IntegrityConflict)
    ));
    // All contenders race through the actual compare-write API. Whichever
    // worker runs first, the highest revision is the only final projection.
    let mut workers = Vec::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(9));
    for revision in 3..=10 {
        let storage = storage.clone();
        let service = service.clone();
        let publisher = publisher.clone();
        let node = node.clone();
        let barrier = barrier.clone();
        workers.push(tokio::spawn(async move {
            barrier.wait().await;
            storage
                .publish_push_item_versioned(
                    &service,
                    &publisher,
                    &node,
                    &item(&revision.to_string(), &revision.to_string()),
                    PublicationVersion::new(revision, uuid::Uuid::new_v4()).expect("version"),
                )
                .await
        }));
    }
    barrier.wait().await;
    for worker in workers {
        worker.await.expect("worker").expect("concurrent write");
    }
    let items = storage
        .get_items(&service, node.as_str(), None, &[])
        .await
        .expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "10");
    storage
        .purge_node(&service, node.as_str())
        .await
        .expect("purge");
    assert!(matches!(
        storage
            .publish_push_item_versioned(&service, &publisher, &node, &latest, version)
            .await
            .expect("purged stale replay"),
        VersionedPublishResult::Superseded
    ));
    assert!(storage
        .get_items(&service, node.as_str(), None, &[])
        .await
        .expect("items")
        .is_empty());
}

#[tokio::test]
async fn versioned_push_pubsub_sqlite_orders_concurrent_retries_and_tombstones() {
    exercise_ordering(Arc::new(
        DatabasePubSubStorage::open(None).await.expect("storage"),
    ))
    .await;
}

#[tokio::test]
async fn versioned_push_pubsub_postgres_orders_concurrent_retries_and_tombstones() {
    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        return;
    };
    let schema = format!("pubsub_versioned_{}", uuid::Uuid::new_v4().simple());
    let admin = sqlx::PgPool::connect(&url).await.expect("admin");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("schema");
    let mut scoped = url::Url::parse(&url).expect("url");
    scoped
        .query_pairs_mut()
        .append_pair("options", &format!("-c search_path={schema}"));
    let storage = Arc::new(
        DatabasePubSubStorage::open(Some(scoped.as_str()))
            .await
            .expect("storage"),
    );
    exercise_ordering(storage.clone()).await;
    exercise_rollback(&storage).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn versioned_push_pubsub_stamp_and_item_roll_back_together() {
    let storage = DatabasePubSubStorage::open(None).await.expect("storage");
    exercise_rollback(&storage).await;
}

async fn exercise_rollback(storage: &DatabasePubSubStorage) {
    let service: BareJid = "push@example.com".parse().expect("service");
    let publisher: BareJid = "alice@example.com".parse().expect("publisher");
    let node = PublicationNode::new("rollback").expect("node");
    storage
        .get_or_create_node(&service, node.as_str())
        .await
        .expect("node");
    let baseline = storage
        .row_count("pubsub_push_publications")
        .await
        .expect("marker baseline");
    match storage.db.driver() {
        crate::db::DatabaseDriver::Sqlite => {
            storage.execute("CREATE TRIGGER fail_push_item BEFORE INSERT ON pubsub_items BEGIN SELECT RAISE(ABORT, 'injected item failure'); END", ()).await.expect("inject failure");
        }
        crate::db::DatabaseDriver::Postgres => {
            storage.execute("CREATE FUNCTION reject_push_item() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected item failure'; END $$", ()).await.expect("failure function");
            storage.execute("CREATE TRIGGER fail_push_item BEFORE INSERT ON pubsub_items FOR EACH ROW EXECUTE FUNCTION reject_push_item()", ()).await.expect("inject failure");
        }
    }
    let version = PublicationVersion::new(1, uuid::Uuid::new_v4()).expect("version");
    let payload = item("approved", "same");
    assert!(storage
        .publish_push_item_versioned(&service, &publisher, &node, &payload, version)
        .await
        .is_err());
    assert_eq!(
        storage
            .row_count("pubsub_push_publications")
            .await
            .expect("marker count"),
        baseline
    );
    assert!(storage
        .get_items(&service, node.as_str(), None, &[])
        .await
        .expect("items")
        .is_empty());
    match storage.db.driver() {
        crate::db::DatabaseDriver::Sqlite => storage
            .execute("DROP TRIGGER fail_push_item", ())
            .await
            .expect("remove injection"),
        crate::db::DatabaseDriver::Postgres => storage
            .execute("DROP TRIGGER fail_push_item ON pubsub_items", ())
            .await
            .expect("remove injection"),
    };
    assert!(matches!(
        storage
            .publish_push_item_versioned(&service, &publisher, &node, &payload, version)
            .await
            .expect("retry"),
        VersionedPublishResult::Applied(_)
    ));
}

#[tokio::test]
async fn versioned_push_pubsub_additive_initialization_preserves_v8_items_and_subscriptions() {
    let storage = DatabasePubSubStorage::open(None).await.expect("storage");
    let service: BareJid = "push@example.com".parse().expect("service");
    let publisher: BareJid = "alice@example.com".parse().expect("publisher");
    let node = PublicationNode::new("existing-v8").expect("node");
    storage
        .get_or_create_node(&service, node.as_str())
        .await
        .expect("node");
    let payload = item("original", "original");
    storage
        .publish_item(&service, node.as_str(), &payload, Some(&publisher), false)
        .await
        .expect("original item");
    let subscriber: jid::Jid = publisher.clone().into();
    storage
        .subscribe(&service, node.as_str(), &subscriber)
        .await
        .expect("subscriber");
    storage
        .execute("DROP TABLE pubsub_push_publications", ())
        .await
        .expect("simulate v8 before additive metadata");
    storage.initialize().await.expect("initialize existing v8");
    assert_eq!(storage.row_count("pubsub_nodes").await.expect("nodes"), 1);
    assert_eq!(
        storage
            .row_count("pubsub_subscriptions")
            .await
            .expect("subscriptions"),
        1
    );
    let original = storage
        .get_items(&service, node.as_str(), None, &[])
        .await
        .expect("original item");
    assert_eq!(original.len(), 1);
    assert_eq!(original[0].to_pubsub_item().payload, payload.payload);
    assert_eq!(
        storage
            .row_count("pubsub_push_publications")
            .await
            .expect("new metadata table"),
        0
    );
    assert!(matches!(
        storage
            .publish_push_item_versioned(
                &service,
                &publisher,
                &node,
                &payload,
                PublicationVersion::new(1, uuid::Uuid::new_v4()).expect("version")
            )
            .await
            .expect("new API"),
        VersionedPublishResult::Applied(_)
    ));
}

#[tokio::test]
async fn versioned_push_pubsub_reopen_preserves_lost_reply_and_retraction_watermark() {
    let artifacts =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts");
    let path = artifacts.join(format!("versioned-push-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let service: BareJid = "push@example.com".parse().expect("service");
    let publisher: BareJid = "alice@example.com".parse().expect("publisher");
    let node = PublicationNode::new("reopen").expect("node");
    let version = PublicationVersion::new(1, uuid::Uuid::new_v4()).expect("version");
    let payload = item("frozen", "same");
    {
        let storage = DatabasePubSubStorage::open(Some(&url))
            .await
            .expect("storage");
        storage
            .get_or_create_node(&service, node.as_str())
            .await
            .expect("node");
        storage
            .publish_push_item_versioned(&service, &publisher, &node, &payload, version)
            .await
            .expect("commit and lose acceptance reply");
    }
    {
        let storage = DatabasePubSubStorage::open(Some(&url))
            .await
            .expect("reopen");
        assert!(matches!(
            storage
                .publish_push_item_versioned(&service, &publisher, &node, &payload, version)
                .await
                .expect("recover same request"),
            VersionedPublishResult::AlreadyApplied
        ));
        storage
            .retract_item(&service, node.as_str(), "same")
            .await
            .expect("retract");
    }
    {
        let storage = DatabasePubSubStorage::open(Some(&url))
            .await
            .expect("reopen retraction");
        assert!(matches!(
            storage
                .publish_push_item_versioned(&service, &publisher, &node, &payload, version)
                .await
                .expect("same request after retraction"),
            VersionedPublishResult::AlreadyApplied
        ));
        assert!(storage
            .get_items(&service, node.as_str(), None, &[])
            .await
            .expect("items")
            .is_empty());
    }
}

async fn assert_digest_only_metadata(
    storage: &DatabasePubSubStorage,
    service: &BareJid,
    node: &PublicationNode,
) {
    let column_query = match storage.db.driver() {
        crate::db::DatabaseDriver::Sqlite => "SELECT name FROM pragma_table_info('pubsub_push_publications') ORDER BY name",
        crate::db::DatabaseDriver::Postgres => "SELECT column_name FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'pubsub_push_publications' ORDER BY column_name",
    };
    let mut rows = storage
        .query(column_query, ())
        .await
        .expect("metadata schema");
    let mut columns: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await.expect("column row") {
        columns.push(row.get(0).expect("column"));
    }
    assert_eq!(
        columns,
        [
            "job_token",
            "node_name",
            "payload_hash",
            "revision",
            "service_jid"
        ]
    );
    let mut rows = storage.query("SELECT payload_hash FROM pubsub_push_publications WHERE service_jid = ? AND node_name = ?",
        crate::db_params![service.to_string(), node.as_str()]).await.expect("fingerprint row");
    let row = rows
        .next()
        .await
        .expect("row")
        .expect("persisted watermark");
    let digest: String = row.get(0).expect("stored digest");
    assert!(digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
}
