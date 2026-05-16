use std::path::PathBuf;

use super::*;

fn jid(value: &str) -> BareJid {
    value.parse().expect("valid JID")
}

fn bookmark_payload() -> minidom::Element {
    "<conference xmlns='urn:xmpp:bookmarks:1' />"
        .parse()
        .expect("valid bookmark payload")
}

#[tokio::test]
async fn database_pubsub_storage_persists_file_backing() {
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let path = artifacts.join(format!("pubsub-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let owner = jid("alice@example.com");

    {
        let storage = DatabasePubSubStorage::open(Some(&url))
            .await
            .expect("storage");
        let (_, created) = storage
            .get_or_create_node(&owner, "urn:xmpp:bookmarks:1")
            .await
            .expect("node");
        assert!(created);

        let item = PubSubItem {
            id: Some("room@muc.example.com".to_string()),
            publisher: None,
            payload: Some(bookmark_payload()),
        };
        storage
            .publish_item(&owner, "urn:xmpp:bookmarks:1", &item, Some(&owner), false)
            .await
            .expect("publish");
    }

    let reopened = DatabasePubSubStorage::open(Some(&url))
        .await
        .expect("reopened storage");
    let items = reopened
        .get_items(&owner, "urn:xmpp:bookmarks:1", None, &[])
        .await
        .expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "room@muc.example.com");

    for cleanup in [
        path.clone(),
        PathBuf::from(format!("{}-shm", path.display())),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let _ = std::fs::remove_file(cleanup);
    }
}

#[tokio::test]
async fn spaces_config_keeps_multiple_items() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("spaces.example.com");
    storage
        .get_or_create_node(&owner, "general")
        .await
        .expect("node");
    storage
        .update_node_config(&owner, "general", &NodeConfig::spaces_public())
        .await
        .expect("config");

    for id in ["one@muc.example.com", "two@muc.example.com"] {
        let item = PubSubItem {
            id: Some(id.to_string()),
            publisher: None,
            payload: Some(bookmark_payload()),
        };
        storage
            .publish_item(&owner, "general", &item, Some(&owner), false)
            .await
            .expect("publish");
    }

    let items = storage
        .get_items(&owner, "general", None, &[])
        .await
        .expect("items");
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn xep0402_bookmark_node_keeps_multiple_items_by_default() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");
    let (node, created) = storage
        .get_or_create_node(&owner, waddle_xmpp::xep::xep0402::PEP_NODE)
        .await
        .expect("node");
    assert!(created);
    assert_eq!(
        node.config.max_items,
        waddle_xmpp::pubsub::PEP_BOOKMARK_MAX_ITEMS
    );

    for id in ["one@muc.example.com", "two@muc.example.com"] {
        let item = PubSubItem {
            id: Some(id.to_string()),
            publisher: None,
            payload: Some(bookmark_payload()),
        };
        storage
            .publish_item(
                &owner,
                waddle_xmpp::xep::xep0402::PEP_NODE,
                &item,
                Some(&owner),
                false,
            )
            .await
            .expect("publish");
    }

    let items = storage
        .get_items(&owner, waddle_xmpp::xep::xep0402::PEP_NODE, None, &[])
        .await
        .expect("items");
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn xep0402_bookmark_node_config_clamps_unbounded_max_items() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");
    let (node, _) = storage
        .get_or_create_node(&owner, waddle_xmpp::xep::xep0402::PEP_NODE)
        .await
        .expect("node");

    let mut config = node.config.clone();
    config.max_items = 0;
    storage
        .update_node_config(&owner, waddle_xmpp::xep::xep0402::PEP_NODE, &config)
        .await
        .expect("clamp zero");
    let node = storage
        .get_node(&owner, waddle_xmpp::xep::xep0402::PEP_NODE)
        .await
        .expect("node lookup")
        .expect("node exists");
    assert_eq!(
        node.config.max_items,
        waddle_xmpp::pubsub::PEP_BOOKMARK_MAX_ITEMS
    );

    let mut config = node.config.clone();
    config.max_items = u32::MAX;
    storage
        .update_node_config(&owner, waddle_xmpp::xep::xep0402::PEP_NODE, &config)
        .await
        .expect("clamp u32 max");
    let node = storage
        .get_node(&owner, waddle_xmpp::xep::xep0402::PEP_NODE)
        .await
        .expect("node lookup")
        .expect("node exists");
    assert_eq!(
        node.config.max_items,
        waddle_xmpp::pubsub::PEP_BOOKMARK_MAX_ITEMS
    );

    let mut config = node.config.clone();
    config.max_items = 10;
    storage
        .update_node_config(&owner, waddle_xmpp::xep::xep0402::PEP_NODE, &config)
        .await
        .expect("allow bounded value");
    let node = storage
        .get_node(&owner, waddle_xmpp::xep::xep0402::PEP_NODE)
        .await
        .expect("node lookup")
        .expect("node exists");
    assert_eq!(node.config.max_items, 10);
}

#[tokio::test]
async fn xep0402_bookmark_node_config_forces_private_durable_fields() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");
    storage
        .get_or_create_node(&owner, waddle_xmpp::xep::xep0402::PEP_NODE)
        .await
        .expect("node");

    storage
        .update_node_config(
            &owner,
            waddle_xmpp::xep::xep0402::PEP_NODE,
            &NodeConfig::spaces_public(),
        )
        .await
        .expect("normalize bookmark config");

    let node = storage
        .get_node(&owner, waddle_xmpp::xep::xep0402::PEP_NODE)
        .await
        .expect("node lookup")
        .expect("node exists");
    assert_eq!(
        node.config.access_model,
        waddle_xmpp::pubsub::AccessModel::Whitelist
    );
    assert_eq!(
        node.config.publish_model,
        waddle_xmpp::pubsub::PublishModel::Publishers
    );
    assert_eq!(
        node.config.max_items,
        waddle_xmpp::pubsub::PEP_BOOKMARK_MAX_ITEMS
    );
    assert!(node.config.persist_items);
    assert_eq!(
        node.config.send_last_published_item,
        waddle_xmpp::pubsub::SendLastPublishedItem::Never
    );
}

#[tokio::test]
async fn pubsub_database_hosts_notification_settings_projection_schema() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let db = storage.database();
    let conn = db.guard().await.expect("guard");

    for column in [
        "owner_bare_jid",
        "conversation_jid",
        "conversation_kind",
        "mode",
        "source_version",
        "updated_at_ms",
        "source_node",
        "source_item_id",
    ] {
        let mut rows = conn
            .query(
                r#"
                SELECT COUNT(*)
                FROM pragma_table_info('notification_settings_projection')
                WHERE name = ?
                "#,
                crate::db_params![column],
            )
            .await
            .expect("column query");
        let row = rows.next().await.expect("row result").expect("row");
        let column_count: i64 = row.get(0).expect("count");
        assert_eq!(column_count, 1, "missing projection column {column}");
    }

    let mut rows = conn
        .query(
            r#"
            SELECT COUNT(*)
            FROM sqlite_master
            WHERE type = 'index'
              AND name = 'idx_notification_settings_projection_owner_mode'
            "#,
            (),
        )
        .await
        .expect("index query");
    let row = rows.next().await.expect("row result").expect("row");
    let owner_mode_indexes: i64 = row.get(0).expect("count");
    assert_eq!(owner_mode_indexes, 1, "owner/mode lookup index must exist");

    conn.execute(
        r#"
        INSERT INTO notification_settings_projection (
            owner_bare_jid,
            conversation_jid,
            conversation_kind,
            mode,
            source_version,
            updated_at_ms,
            source_node,
            source_item_id
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        "#,
        crate::db_params![
            "alice@example.com",
            "room@muc.example.com",
            "private_group",
            "on-mention",
            1_i64,
            7_i64,
            "urn:xmpp:bookmarks:1",
            "room@muc.example.com",
        ],
    )
    .await
    .expect("valid projection row");

    let invalid_mode = conn
        .execute(
            r#"
            INSERT INTO notification_settings_projection (
                owner_bare_jid,
                conversation_jid,
                conversation_kind,
                mode,
                source_version,
                updated_at_ms,
                source_node,
                source_item_id
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "alice@example.com",
                "mode-error@muc.example.com",
                "private_group",
                "mentions-only",
                1_i64,
                7_i64,
                "urn:xmpp:bookmarks:1",
                "mode-error@muc.example.com",
            ],
        )
        .await;
    assert!(
        invalid_mode.is_err(),
        "projection mode CHECK must reject non-XEP-0492 values"
    );

    let invalid_kind = conn
        .execute(
            r#"
            INSERT INTO notification_settings_projection (
                owner_bare_jid,
                conversation_jid,
                conversation_kind,
                mode,
                source_version,
                updated_at_ms,
                source_node,
                source_item_id
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                "alice@example.com",
                "kind-error@muc.example.com",
                "channel",
                "never",
                1_i64,
                7_i64,
                "urn:xmpp:bookmarks:1",
                "kind-error@muc.example.com",
            ],
        )
        .await;
    assert!(
        invalid_kind.is_err(),
        "projection conversation_kind CHECK must reject unknown kinds"
    );
}

#[tokio::test]
async fn pubsub_postgres_projection_schema_uses_bigint_revision_columns() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (postgres-backed PubSub projection schema regression)"
        );
        return;
    };

    let schema = unique_postgres_schema_name("pubsub_projection");
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect postgres admin pool");
    let create_schema = format!("CREATE SCHEMA {schema}");
    sqlx::query(&create_schema)
        .execute(&admin)
        .await
        .expect("create isolated schema");

    let scoped_url = postgres_url_with_search_path(&database_url, &schema);
    let storage = DatabasePubSubStorage::open(Some(&scoped_url))
        .await
        .expect("postgres pubsub storage");
    let db = storage.database();
    let conn = db.guard().await.expect("postgres guard");
    for column in ["source_version", "updated_at_ms"] {
        let mut rows = conn
            .query(
                "SELECT data_type \
                 FROM information_schema.columns \
                 WHERE table_schema = current_schema() \
                   AND table_name = ? \
                   AND column_name = ?",
                crate::db_params!["notification_settings_projection", column],
            )
            .await
            .expect("query projection revision column");
        let row = rows
            .next()
            .await
            .expect("read column type")
            .expect("column row");
        let data_type: String = row.get(0).expect("decode column type");
        assert_eq!(data_type, "bigint", "projection {column} must use BIGINT");
    }
    drop(conn);

    let drop_schema = format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
    sqlx::query(&drop_schema)
        .execute(&admin)
        .await
        .expect("drop isolated schema");
}

fn unique_postgres_schema_name(prefix: &str) -> String {
    format!("waddle_test_{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres url");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

#[tokio::test]
async fn database_subscriptions_persist_across_reopen() {
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts");
    let path = artifacts.join(format!("pubsub-sub-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let owner = jid("alice@example.com");
    let alice: jid::Jid = "alice@example.com".parse().expect("jid");

    let saved_subid = {
        let storage = DatabasePubSubStorage::open(Some(&url))
            .await
            .expect("storage");
        storage.get_or_create_node(&owner, "n").await.expect("node");
        let sub = storage
            .subscribe(&owner, "n", &alice)
            .await
            .expect("subscribe");
        sub.subid
    };

    let reopened = DatabasePubSubStorage::open(Some(&url))
        .await
        .expect("reopen");
    let listed = reopened
        .list_node_subscriptions(&owner, "n")
        .await
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].subid, saved_subid);

    for cleanup in [
        path.clone(),
        PathBuf::from(format!("{}-shm", path.display())),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let _ = std::fs::remove_file(cleanup);
    }
}

#[tokio::test]
async fn database_affiliations_persist_across_reopen() {
    let artifacts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts");
    let path = artifacts.join(format!("pubsub-aff-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let owner = jid("alice@example.com");
    let bob = jid("bob@example.com");

    {
        let storage = DatabasePubSubStorage::open(Some(&url))
            .await
            .expect("storage");
        storage.get_or_create_node(&owner, "n").await.expect("node");
        let prev = storage
            .set_affiliation(&owner, "n", &bob, Affiliation::Outcast)
            .await
            .expect("set");
        assert_eq!(prev, Affiliation::None);
    }

    let reopened = DatabasePubSubStorage::open(Some(&url))
        .await
        .expect("reopen");
    let aff = reopened
        .get_affiliation(&owner, "n", &bob)
        .await
        .expect("get");
    assert_eq!(aff, Affiliation::Outcast);

    for cleanup in [
        path.clone(),
        PathBuf::from(format!("{}-shm", path.display())),
        PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let _ = std::fs::remove_file(cleanup);
    }
}

#[tokio::test]
async fn database_purge_clears_items_keeps_node() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");
    storage.get_or_create_node(&owner, "n").await.expect("node");
    storage
        .update_node_config(&owner, "n", &NodeConfig::spaces_public())
        .await
        .expect("config");
    for i in 1..=3 {
        let item = PubSubItem {
            id: Some(format!("i{i}")),
            publisher: None,
            payload: None,
        };
        storage
            .publish_item(&owner, "n", &item, None, false)
            .await
            .expect("publish");
    }
    let purged = storage.purge_node(&owner, "n").await.expect("purge");
    assert_eq!(purged, 3);
    assert!(storage.get_node(&owner, "n").await.expect("get").is_some());
    assert!(storage
        .get_items(&owner, "n", None, &[])
        .await
        .expect("items")
        .is_empty());
}

#[tokio::test]
async fn build_pubsub_storage_envvar_gating() {
    let prior = std::env::var("WADDLE_PUBSUB_INMEMORY").ok();

    std::env::remove_var("WADDLE_PUBSUB_INMEMORY");
    let no_env = build_pubsub_storage(None).await;
    assert!(
        no_env.is_err(),
        "expected error without URL and without env var"
    );

    std::env::set_var("WADDLE_PUBSUB_INMEMORY", "1");
    let with_env = build_pubsub_storage(None).await;
    assert!(with_env.is_ok(), "expected success with env var set");

    match prior {
        Some(value) => std::env::set_var("WADDLE_PUBSUB_INMEMORY", value),
        None => std::env::remove_var("WADDLE_PUBSUB_INMEMORY"),
    }
}

#[tokio::test]
async fn database_deliverable_subscribers_excludes_outcast() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");
    storage.get_or_create_node(&owner, "n").await.expect("node");
    let alice: jid::Jid = "alice@x.com".parse().expect("jid");
    let bob: jid::Jid = "bob@x.com".parse().expect("jid");
    storage
        .subscribe(&owner, "n", &alice)
        .await
        .expect("subscribe");
    storage
        .subscribe(&owner, "n", &bob)
        .await
        .expect("subscribe");
    let bob_bare = jid("bob@x.com");
    storage
        .set_affiliation(&owner, "n", &bob_bare, Affiliation::Outcast)
        .await
        .expect("set");

    let listed = storage
        .list_deliverable_subscribers(&owner, "n")
        .await
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].subscriber.to_string(), "alice@x.com");
}
