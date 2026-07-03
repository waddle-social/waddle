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

fn dnd_payload() -> minidom::Element {
    "<dnd xmlns='urn:waddle:dnd:0' timezone='UTC'/>"
        .parse()
        .expect("valid dnd payload")
}

fn story_payload(body: &str) -> minidom::Element {
    format!(
        "<entry xmlns='http://www.w3.org/2005/Atom'><title type='text'>{body}</title><id>story-test</id><updated>2026-06-01T12:00:00Z</updated><content type='text'>{body}</content><link rel='enclosure' href='https://example.com/story-test.jpg' type='image/jpeg'/></entry>"
    )
    .parse()
    .expect("valid story payload")
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
async fn database_node_config_persists_pubsub_type() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("community.example.com");
    let node = waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES;
    storage
        .get_or_create_node(&owner, node)
        .await
        .expect("node");
    storage
        .update_node_config(&owner, node, &NodeConfig::community_stories())
        .await
        .expect("config");

    let stored = storage
        .get_node(&owner, node)
        .await
        .expect("get node")
        .expect("node exists");
    assert_eq!(
        stored.config.node_type,
        Some(waddle_xmpp_core::pubsub::PubSubNodeType::PubsubSocialFeedStories)
    );
}

#[tokio::test]
async fn publish_item_if_missing_or_publisher_rejects_different_publisher() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("community.example.com");
    let alice = jid("alice@example.com");
    let bob = jid("bob@example.com");
    storage
        .get_or_create_node(&owner, "stories")
        .await
        .expect("node");

    let alice_item = PubSubItem {
        id: Some("story-1".to_string()),
        publisher: None,
        payload: Some(story_payload("first")),
    };
    storage
        .publish_item_if_missing_or_publisher(&owner, "stories", &alice_item, &alice, false)
        .await
        .expect("alice publish");

    let alice_update = PubSubItem {
        id: Some("story-1".to_string()),
        publisher: None,
        payload: Some(story_payload("updated")),
    };
    storage
        .publish_item_if_missing_or_publisher(&owner, "stories", &alice_update, &alice, false)
        .await
        .expect("same publisher update");

    let bob_update = PubSubItem {
        id: Some("story-1".to_string()),
        publisher: None,
        payload: Some(story_payload("clobber")),
    };
    let error = storage
        .publish_item_if_missing_or_publisher(&owner, "stories", &bob_update, &bob, false)
        .await
        .expect_err("different publisher must be rejected");
    assert!(
        matches!(
            error,
            waddle_xmpp::XmppError::Stanza {
                condition: waddle_xmpp::StanzaErrorCondition::Forbidden,
                ..
            }
        ),
        "unexpected error for cross-publisher clobber: {error:?}"
    );

    let items = storage
        .get_items(&owner, "stories", None, &["story-1".to_string()])
        .await
        .expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].publisher.as_ref(), Some(&alice));
    assert!(
        items[0]
            .payload_xml
            .as_deref()
            .is_some_and(|payload| payload.contains("updated")),
        "cross-publisher publish must not replace existing payload: {items:?}"
    );
}

#[tokio::test]
async fn republish_refreshes_retention_order() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");
    storage.get_or_create_node(&owner, "n").await.expect("node");

    for id in ["old", "new", "old"] {
        let item = PubSubItem {
            id: Some(id.to_string()),
            publisher: None,
            payload: None,
        };
        storage
            .publish_item(&owner, "n", &item, Some(&owner), false)
            .await
            .expect("publish");
    }

    let items = storage
        .get_items(&owner, "n", None, &[])
        .await
        .expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "old");
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
async fn dm_bookmark_node_config_clamps_unbounded_max_items_and_pins_whitelist() {
    // Parity with the XEP-0402 bookmarks node: an owner reconfiguring
    // their own urn:waddle:dm-bookmarks:0 node MUST NOT be able to set
    // max_items=max (which would re-disable eviction, the anti-DoS hole
    // from the adversarial review) or access_model=open (which would
    // leak which contacts they have muted to roster peers).
    use waddle_xmpp_core::pubsub::AccessModel;
    let dm_node = waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");
    let (node, _) = storage
        .get_or_create_node(&owner, dm_node)
        .await
        .expect("node");

    let mut config = node.config.clone();
    config.max_items = u32::MAX;
    config.access_model = AccessModel::Open;
    storage
        .update_node_config(&owner, dm_node, &config)
        .await
        .expect("reconfigure");
    let node = storage
        .get_node(&owner, dm_node)
        .await
        .expect("node lookup")
        .expect("node exists");
    assert_eq!(
        node.config.max_items,
        waddle_xmpp::pubsub::PEP_BOOKMARK_MAX_ITEMS,
        "unbounded max_items must clamp to the anti-DoS cap"
    );
    assert_eq!(
        node.config.access_model,
        AccessModel::Whitelist,
        "access_model must be forced back to whitelist (privacy)"
    );

    // A bounded value within the cap is preserved.
    let mut config = node.config.clone();
    config.max_items = 8;
    storage
        .update_node_config(&owner, dm_node, &config)
        .await
        .expect("allow bounded value");
    let node = storage
        .get_node(&owner, dm_node)
        .await
        .expect("node lookup")
        .expect("node exists");
    assert_eq!(node.config.max_items, 8);
}

#[tokio::test]
async fn well_known_private_pep_nodes_pin_whitelist_across_reconfigure() {
    // Issue #1094: fan-out privacy is derived from the stored
    // access_model, so every well-known whitelist PEP node must resist
    // an owner configure-set flipping it open — otherwise one IQ
    // re-enables the roster fan-out and non-owner item reads the
    // whitelist default exists to prevent.
    use waddle_xmpp_core::pubsub::AccessModel;
    let private_nodes = [
        waddle_xmpp_core::pubsub::pep::PEP_NODE_MDS_DISPLAYED,
        waddle_xmpp_core::pubsub::pep::PEP_NODE_WADDLE_DND,
        waddle_xmpp_core::waddle_story_reads::PEP_NODE_WADDLE_STORY_READS,
        waddle_xmpp_core::waddle_status_preference::PEP_NODE_WADDLE_STATUS_PREFERENCE,
    ];

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");

    for pep_node in private_nodes {
        let (node, _) = storage
            .get_or_create_node(&owner, pep_node)
            .await
            .expect("node");
        assert_eq!(
            node.config.access_model,
            AccessModel::Whitelist,
            "{pep_node} must auto-create whitelist"
        );

        let mut config = node.config.clone();
        config.access_model = AccessModel::Open;
        storage
            .update_node_config(&owner, pep_node, &config)
            .await
            .expect("reconfigure");
        let node = storage
            .get_node(&owner, pep_node)
            .await
            .expect("node lookup")
            .expect("node exists");
        assert_eq!(
            node.config.access_model,
            AccessModel::Whitelist,
            "{pep_node} access_model must be forced back to whitelist"
        );
    }
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

/// Re-subscribing with the same `(owner, node, subscriber)` MUST be a
/// no-op — return the existing subscription's `subid` and leave the
/// row count at one. Without this, the chat's bind-time
/// `<subscribe/>` to e.g. `urn:xmpp:mds:displayed:0` inserts a fresh
/// row on every reconnect; production observed 490 rows for a single
/// user/node pair, blowing the fanout's per-recipient outbound
/// channel and dropping most stanzas (the chat-side reconcile-to-
/// "delivered" never fires).
#[tokio::test]
async fn subscribe_is_idempotent_for_same_owner_node_subscriber_bare() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");
    storage
        .get_or_create_node(&owner, "urn:xmpp:mds:displayed:0")
        .await
        .expect("node");
    let alice_full: jid::Jid = "alice@example.com/web-1".parse().expect("jid");

    let first = storage
        .subscribe(&owner, "urn:xmpp:mds:displayed:0", &alice_full)
        .await
        .expect("first subscribe");
    let second = storage
        .subscribe(&owner, "urn:xmpp:mds:displayed:0", &alice_full)
        .await
        .expect("second subscribe");
    assert_eq!(
        first.subid.as_str(),
        second.subid.as_str(),
        "re-subscribe must return the original subid, not mint a new one"
    );

    let listed = storage
        .list_deliverable_subscribers(&owner, "urn:xmpp:mds:displayed:0")
        .await
        .expect("list");
    assert_eq!(
        listed.len(),
        1,
        "re-subscribe must NOT add another row for the same (owner, node, subscriber_bare)"
    );
}

/// Re-subscribing with a different resource on the same bare JID must
/// also collapse to one row — full-JID subscriptions aren't stored,
/// and matching `list_deliverable_subscribers` semantics requires the
/// bare-JID dedupe to span resources.
#[tokio::test]
async fn subscribe_is_idempotent_across_resources_for_same_bare_jid() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");
    storage
        .get_or_create_node(&owner, "urn:xmpp:mds:displayed:0")
        .await
        .expect("node");

    for resource in ["web-1", "web-2", "web-3", "desktop"] {
        let jid_with_resource: jid::Jid = format!("alice@example.com/{resource}")
            .parse()
            .expect("jid");
        storage
            .subscribe(&owner, "urn:xmpp:mds:displayed:0", &jid_with_resource)
            .await
            .expect("subscribe");
    }

    let listed = storage
        .list_deliverable_subscribers(&owner, "urn:xmpp:mds:displayed:0")
        .await
        .expect("list");
    assert_eq!(
        listed.len(),
        1,
        "four resource-suffixed subscribes to the same bare JID must collapse to one row"
    );
}

/// The migration path: when prod already has duplicate rows from
/// before this fix (the icepuma/mds:displayed:0 incident left 490
/// rows), the next `subscribe` call MUST notice and delete them. The
/// rows aren't separately schema-migrated; lazy cleanup on each
/// touch is the recovery contract.
#[tokio::test]
async fn subscribe_cleans_up_pre_existing_duplicate_rows() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let owner = jid("alice@example.com");
    storage
        .get_or_create_node(&owner, "urn:xmpp:mds:displayed:0")
        .await
        .expect("node");

    // Simulate the leak: insert five raw duplicate rows for the same
    // (owner, node, subscriber_bare) triple — what the pre-fix
    // `subscribe` path would have produced over five reconnects.
    let alice_bare = "alice@example.com";
    for (i, subid) in ["a", "b", "c", "d", "e"].iter().enumerate() {
        storage
            .execute(
                "INSERT INTO pubsub_subscriptions (owner_jid, node_name, subid, subscriber_jid, state, created_at_ms) \
                 VALUES (?, ?, ?, ?, 'subscribed', ?)",
                crate::db_params![
                    owner.to_string(),
                    "urn:xmpp:mds:displayed:0".to_string(),
                    subid.to_string(),
                    alice_bare.to_string(),
                    1_000_000_i64 + i as i64,
                ],
            )
            .await
            .expect("insert raw duplicate");
    }
    let pre = storage
        .list_deliverable_subscribers(&owner, "urn:xmpp:mds:displayed:0")
        .await
        .expect("list before");
    assert_eq!(pre.len(), 5, "leak scenario must seed five duplicates");

    // Next subscribe call MUST collapse the backlog down to the
    // oldest row.
    let alice: jid::Jid = "alice@example.com/web-now".parse().expect("jid");
    let kept = storage
        .subscribe(&owner, "urn:xmpp:mds:displayed:0", &alice)
        .await
        .expect("subscribe");
    assert_eq!(
        kept.subid.as_str(),
        "a",
        "subscribe must keep the oldest row by created_at_ms, not mint a new one"
    );

    let post = storage
        .list_deliverable_subscribers(&owner, "urn:xmpp:mds:displayed:0")
        .await
        .expect("list after");
    assert_eq!(
        post.len(),
        1,
        "subscribe must garbage-collect duplicate rows for the (owner, node, subscriber) triple"
    );
}

/// The XEP-0163 single-item PEP convention is `id="current"`. A
/// client that publishes with a different id (e.g. `id="custom"`)
/// then later retracts `id="current"` would silently leave the
/// projection in place — the user would stay in DND with no
/// wire-level way to clear it. Reject up-front. Copilot review on
/// PR #759.
#[tokio::test]
async fn dnd_publish_with_wrong_item_id_is_bad_request() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let bob = jid("bob@example.com");
    storage
        .get_or_create_node(&bob, "urn:waddle:dnd:0")
        .await
        .expect("node");

    let bad_item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("custom".to_string()),
        publisher: None,
        payload: Some(dnd_payload()),
    };
    let err = storage
        .publish_item(&bob, "urn:waddle:dnd:0", &bad_item, Some(&bob), false)
        .await
        .expect_err("non-current item id must error");
    assert!(
        matches!(err, waddle_xmpp::XmppError::PubSubInvalidPayload(_)),
        "expected PubSubInvalidPayload (XEP-0060 §7.1.3.4), got: {err:?}"
    );

    let missing_id = waddle_xmpp::pubsub::PubSubItem {
        id: None,
        publisher: None,
        payload: Some(dnd_payload()),
    };
    let err = storage
        .publish_item(&bob, "urn:waddle:dnd:0", &missing_id, Some(&bob), false)
        .await
        .expect_err("missing item id must also error");
    assert!(matches!(
        err,
        waddle_xmpp::XmppError::PubSubInvalidPayload(_)
    ));

    // The `current` id is accepted.
    let good_item = waddle_xmpp::pubsub::PubSubItem {
        id: Some(waddle_xmpp::xep::xep_waddle_dnd::ITEM_ID_CURRENT.to_string()),
        publisher: None,
        payload: Some(dnd_payload()),
    };
    storage
        .publish_item(&bob, "urn:waddle:dnd:0", &good_item, Some(&bob), false)
        .await
        .expect("current id should be accepted");
}

/// The DND projection's `source_version` MUST come from the
/// monotonic `dnd_projection_source_version` counter, not from
/// wall-clock time. After two back-to-back publishes the counter
/// MUST have incremented by at least one — proving the LWW guard
/// is driven by transaction-serialized monotonicity, not by
/// (potentially regressing) NTP time. Round-7 Copilot review on PR
/// #759.
#[tokio::test]
async fn dnd_publish_source_version_is_strictly_monotonic_per_publish() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let bob = jid("bob@example.com");
    let projection_store = crate::dnd_projection::DndProjectionStore::new(storage.database());

    let item = waddle_xmpp::pubsub::PubSubItem {
        id: Some(waddle_xmpp::xep::xep_waddle_dnd::ITEM_ID_CURRENT.to_string()),
        publisher: None,
        payload: Some(dnd_payload()),
    };
    storage
        .get_or_create_node(&bob, "urn:waddle:dnd:0")
        .await
        .expect("node");
    storage
        .publish_item(&bob, "urn:waddle:dnd:0", &item, Some(&bob), false)
        .await
        .expect("first publish");
    let first = projection_store
        .get(&bob)
        .await
        .expect("get")
        .expect("row")
        .source_version;
    storage
        .publish_item(&bob, "urn:waddle:dnd:0", &item, Some(&bob), false)
        .await
        .expect("second publish");
    let second = projection_store
        .get(&bob)
        .await
        .expect("get")
        .expect("row")
        .source_version;
    assert!(
        second > first,
        "source_version must be strictly increasing across publishes \
         (first={first}, second={second})"
    );
}

/// A DND publish without any `<dnd>` payload (XEP-0060 §7.1.3.3) MUST
/// surface as the typed `PubSubPayloadRequired` variant — distinct
/// from the malformed-payload case (§7.1.3.4). The dispatch layer
/// maps the two onto different pubsub-error extension elements on
/// the wire, so the typed discriminator is load-bearing.
#[tokio::test]
async fn dnd_publish_without_payload_is_payload_required() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let bob = jid("bob@example.com");
    storage
        .get_or_create_node(&bob, "urn:waddle:dnd:0")
        .await
        .expect("node");

    let item_missing_payload = waddle_xmpp::pubsub::PubSubItem {
        id: Some(waddle_xmpp::xep::xep_waddle_dnd::ITEM_ID_CURRENT.to_string()),
        publisher: None,
        payload: None,
    };
    let err = storage
        .publish_item(
            &bob,
            "urn:waddle:dnd:0",
            &item_missing_payload,
            Some(&bob),
            false,
        )
        .await
        .expect_err("missing payload must error");
    assert!(
        matches!(err, waddle_xmpp::XmppError::PubSubPayloadRequired(_)),
        "expected PubSubPayloadRequired (XEP-0060 §7.1.3.3), got: {err:?}"
    );
}

/// A publisher that is NOT the node owner MUST be rejected with
/// `<forbidden/>` when targeting `urn:waddle:dnd:0`. Accepting the
/// publish-only path while skipping the projection write would
/// leave `pubsub_items` and `dnd_projection` in disagreement —
/// subscribers fetching `<items/>` would see the peer's spoofed
/// payload while the T1 push gate consults the owner's last-known
/// state. Found by the round-3 hostile-client adversarial review.
#[tokio::test]
async fn dnd_publish_from_non_owner_is_forbidden() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let bob = jid("bob@example.com");
    let mallory = jid("mallory@example.com");
    storage
        .get_or_create_node(&bob, "urn:waddle:dnd:0")
        .await
        .expect("node");

    let item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("current".to_string()),
        publisher: None,
        payload: Some(dnd_payload()),
    };

    let result = storage
        .publish_item(&bob, "urn:waddle:dnd:0", &item, Some(&mallory), false)
        .await;
    let err = result.expect_err("non-owner DND publish must error");
    match &err {
        waddle_xmpp::XmppError::Stanza { condition, .. } => {
            assert!(
                format!("{condition:?}")
                    .to_lowercase()
                    .contains("forbidden"),
                "expected <forbidden/> condition, got: {condition:?}"
            );
        }
        other => panic!("expected Stanza forbidden error, got: {other:?}"),
    }

    // Sanity check: the owner can still publish.
    storage
        .publish_item(&bob, "urn:waddle:dnd:0", &item, Some(&bob), false)
        .await
        .expect("owner publish should succeed");
}

fn dm_bookmark_payload(inner: &str) -> minidom::Element {
    format!("<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>{inner}</dm-bookmark>")
        .parse()
        .expect("valid dm-bookmark payload")
}

/// End-to-end at the store layer for the Waddle DM carrier
/// (`urn:waddle:dm-bookmarks:0`): publishing a `<dm-bookmark>` with a
/// `<never/>` override writes a `Direct` projection row keyed on the
/// contact JID, and retracting that item clears the row in the same tx.
#[tokio::test]
async fn dm_bookmark_publish_then_retract_round_trips_direct_projection() {
    use crate::notification_settings_projection::{
        ConversationKind, NotificationSettingsProjectionStore, NotificationSettingsSource,
    };
    use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let projection = NotificationSettingsProjectionStore::new(storage.database());
    let alice = jid("alice@example.com");
    let bob = jid("bob@example.com");

    storage
        .get_or_create_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("dm-bookmarks node");

    let item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(dm_bookmark_payload(
            "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>",
        )),
    };
    storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &item,
            Some(&alice),
            false,
        )
        .await
        .expect("dm-bookmark publish");

    let row = projection
        .get(&alice, &bob)
        .await
        .expect("get projection")
        .expect("projection row present after publish");
    assert_eq!(row.conversation_kind, ConversationKind::Direct);
    assert_eq!(
        row.mode,
        waddle_xmpp::xep::NotificationLevel::Never,
        "DM <never/> override must persist as Never"
    );
    assert_eq!(row.source, NotificationSettingsSource::WaddleDmBookmarks);
    assert_eq!(row.source_item_jid, bob);

    let retracted = storage
        .retract_item(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS, "bob@example.com")
        .await
        .expect("retract");
    assert!(retracted, "the published DM item must be retracted");
    assert!(
        projection
            .get(&alice, &bob)
            .await
            .expect("get projection after retract")
            .is_none(),
        "retracting the DM item must clear its Direct projection row"
    );
}

/// Republishing a DM bookmark with an EMPTY `<dm-bookmark/>` (no
/// `<notify>` — "no override") deletes any existing Direct projection
/// row in the same publish tx, mirroring the XEP-0402 missing-notify
/// behavior.
#[tokio::test]
async fn dm_bookmark_publish_without_notify_clears_direct_projection() {
    use crate::notification_settings_projection::NotificationSettingsProjectionStore;
    use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let projection = NotificationSettingsProjectionStore::new(storage.database());
    let alice = jid("alice@example.com");
    let bob = jid("bob@example.com");

    storage
        .get_or_create_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("dm-bookmarks node");

    let with_override = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(dm_bookmark_payload(
            "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>",
        )),
    };
    storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &with_override,
            Some(&alice),
            false,
        )
        .await
        .expect("publish override");
    assert!(projection.get(&alice, &bob).await.expect("get").is_some());

    let cleared = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(dm_bookmark_payload("")),
    };
    storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &cleared,
            Some(&alice),
            false,
        )
        .await
        .expect("publish cleared override");
    assert!(
        projection
            .get(&alice, &bob)
            .await
            .expect("get after clear")
            .is_none(),
        "an empty <dm-bookmark/> publish must delete the Direct projection row"
    );
}

/// A malformed `<dm-bookmark>` publish (two account-wide fallbacks in
/// the hosted `<notify>`) MUST be rejected with `<bad-request/>` and
/// leave no `pubsub_items` row behind.
#[tokio::test]
async fn dm_bookmark_publish_with_malformed_notify_is_bad_request() {
    use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let alice = jid("alice@example.com");
    storage
        .get_or_create_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("dm-bookmarks node");

    let item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(dm_bookmark_payload(
            "<notify xmlns='urn:xmpp:notification-settings:1'><always /><never /></notify>",
        )),
    };
    let err = storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &item,
            Some(&alice),
            false,
        )
        .await
        .expect_err("malformed DM bookmark must be rejected");
    match &err {
        waddle_xmpp::XmppError::Stanza { condition, .. } => {
            assert!(
                format!("{condition:?}")
                    .to_lowercase()
                    .contains("badrequest"),
                "expected <bad-request/> condition, got: {condition:?}"
            );
        }
        other => panic!("expected Stanza bad-request error, got: {other:?}"),
    }

    let stored = storage
        .get_items(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS, None, &[])
        .await
        .expect("get items");
    assert!(
        stored.is_empty(),
        "a rejected DM publish must not leave a pubsub_items row"
    );
}

/// A DM-bookmark publish that omits the PubSub item id is rejected with a
/// carrier-specific `<bad-request/>` — the id MUST be the contact bare
/// JID — rather than the confusing "invalid JID: <uuid>" the item_id
/// UUID fallback would otherwise surface downstream (Copilot review).
#[tokio::test]
async fn dm_bookmark_publish_without_item_id_is_bad_request() {
    use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let alice = jid("alice@example.com");
    storage
        .get_or_create_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("dm-bookmarks node");

    let item = waddle_xmpp::pubsub::PubSubItem {
        id: None,
        publisher: None,
        payload: Some(dm_bookmark_payload(
            "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>",
        )),
    };
    let err = storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &item,
            Some(&alice),
            false,
        )
        .await
        .expect_err("DM publish without an item id must be rejected");
    match &err {
        waddle_xmpp::XmppError::Stanza { condition, .. } => {
            assert!(
                format!("{condition:?}")
                    .to_lowercase()
                    .contains("badrequest"),
                "expected <bad-request/> condition, got: {condition:?}"
            );
        }
        other => panic!("expected Stanza bad-request error, got: {other:?}"),
    }

    let stored = storage
        .get_items(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS, None, &[])
        .await
        .expect("get items");
    assert!(
        stored.is_empty(),
        "a rejected DM publish must not leave a pubsub_items row"
    );
}

/// Purging the DM-bookmarks node clears its Direct projection rows but
/// leaves the XEP-0402 MUC carrier's rows untouched (the purge keys on
/// `source_node`, not the conversation JID).
#[tokio::test]
async fn dm_bookmark_purge_clears_only_direct_projection_rows() {
    use crate::notification_settings_projection::{
        ConversationKind, NotificationSettingsProjection, NotificationSettingsProjectionStore,
        NotificationSettingsSource,
    };
    use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let projection = NotificationSettingsProjectionStore::new(storage.database());
    let alice = jid("alice@example.com");
    let bob = jid("bob@example.com");
    let room = jid("room@muc.example.com");

    // Seed a MUC (XEP-0402) projection row directly.
    projection
        .upsert(&NotificationSettingsProjection {
            owner_bare_jid: alice.clone(),
            conversation_jid: room.clone(),
            conversation_kind: ConversationKind::PrivateGroup,
            mode: waddle_xmpp::xep::NotificationLevel::Never,
            rich_payload_opt_in: false,
            source_version: 1,
            updated_at_ms: 1,
            source: NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: room.clone(),
        })
        .await
        .expect("seed muc row");

    // Publish a DM override so a Direct row exists.
    storage
        .get_or_create_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("dm-bookmarks node");
    let item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(dm_bookmark_payload(
            "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>",
        )),
    };
    storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &item,
            Some(&alice),
            false,
        )
        .await
        .expect("publish dm override");
    assert!(projection
        .get(&alice, &bob)
        .await
        .expect("dm row")
        .is_some());

    storage
        .purge_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("purge dm node");

    assert!(
        projection
            .get(&alice, &bob)
            .await
            .expect("dm row after purge")
            .is_none(),
        "purging the DM node must clear its Direct projection rows"
    );
    assert!(
        projection
            .get(&alice, &room)
            .await
            .expect("muc row after purge")
            .is_some(),
        "purging the DM node must NOT touch XEP-0402 MUC projection rows"
    );
}

/// A XEP-0402 `<conference>` payload carrying a `<notify>` extension with
/// the given fallback child name (`always` / `on-mention` / `never`).
fn conference_with_notify(fallback: &str) -> minidom::Element {
    format!(
        "<conference xmlns='urn:xmpp:bookmarks:1'>\
            <extensions>\
                <notify xmlns='urn:xmpp:notification-settings:1'><{fallback}/></notify>\
            </extensions>\
        </conference>"
    )
    .parse()
    .expect("valid conference payload")
}

/// Regression for the source-agnostic projection-delete clobber (FIX 1,
/// #720 adversarial review). The projection PK
/// `(owner_bare_jid, conversation_jid)` is source-agnostic, and the
/// XEP-0402 validator accepts any bare JID with a localpart as a
/// conference bookmark id — so a MUC bookmark id can EQUAL a DM peer JID
/// and both carriers target the SAME projection row.
///
/// Scenario: publish a DM `<never/>` override for `bob@example.com`
/// (row source = WaddleDmBookmarks); then publish a XEP-0402 conference
/// bookmark with item id `bob@example.com` carrying `<on-mention/>`
/// (OVERWRITES the shared row → source becomes Xep0402Bookmarks, mode
/// OnMention); then RETRACT the DM item `bob@example.com`. The
/// source-scoped delete finds no WaddleDmBookmarks row, so the MUC-sourced
/// row MUST survive. Without the fix the source-agnostic delete would
/// wrongly wipe the MUC row.
#[tokio::test]
async fn dm_retract_does_not_clobber_muc_projection_row_for_same_jid() {
    use crate::notification_settings_projection::{
        NotificationSettingsProjectionStore, NotificationSettingsSource,
    };
    use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let projection = NotificationSettingsProjectionStore::new(storage.database());
    let alice = jid("alice@example.com");
    let bob = jid("bob@example.com");

    // 1) DM override for bob → WaddleDmBookmarks row, mode Never.
    storage
        .get_or_create_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("dm node");
    let dm_item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(dm_bookmark_payload(
            "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>",
        )),
    };
    storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &dm_item,
            Some(&alice),
            false,
        )
        .await
        .expect("dm publish");
    let row = projection
        .get(&alice, &bob)
        .await
        .expect("get")
        .expect("dm row present");
    assert_eq!(row.source, NotificationSettingsSource::WaddleDmBookmarks);

    // 2) XEP-0402 conference bookmark with the SAME item id bob@example.com
    //    carrying <on-mention/> → OVERWRITES the shared projection row.
    storage
        .get_or_create_node(&alice, waddle_xmpp::xep::xep0402::PEP_NODE)
        .await
        .expect("bookmarks node");
    let muc_item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(conference_with_notify("on-mention")),
    };
    storage
        .publish_item(
            &alice,
            waddle_xmpp::xep::xep0402::PEP_NODE,
            &muc_item,
            Some(&alice),
            false,
        )
        .await
        .expect("muc publish");
    let row = projection
        .get(&alice, &bob)
        .await
        .expect("get")
        .expect("row present after overwrite");
    assert_eq!(
        row.source,
        NotificationSettingsSource::Xep0402Bookmarks,
        "the MUC publish must overwrite the shared row's source"
    );
    assert_eq!(
        row.mode,
        waddle_xmpp::xep::NotificationLevel::OnMention,
        "the MUC publish must set the shared row's mode"
    );

    // 3) Retract the DM item bob@example.com. The source-scoped delete
    //    must NOT touch the Xep0402Bookmarks-sourced row.
    let retracted = storage
        .retract_item(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS, "bob@example.com")
        .await
        .expect("retract dm item");
    assert!(retracted, "the DM pubsub item must be retracted");

    let surviving = projection
        .get(&alice, &bob)
        .await
        .expect("get after dm retract")
        .expect("MUC-sourced projection row must survive the DM retract");
    assert_eq!(
        surviving.source,
        NotificationSettingsSource::Xep0402Bookmarks,
        "the source-scoped delete must leave the MUC row intact"
    );
    assert_eq!(
        surviving.mode,
        waddle_xmpp::xep::NotificationLevel::OnMention,
    );
}

/// A DM publish that derives a `Delete` mutation (an empty `<dm-bookmark/>`
/// with no `<notify>`, i.e. the override was cleared) MUST NOT clobber a
/// XEP-0402-sourced projection row for the same `conversation_jid`. This
/// covers the publish→`Delete` path through `apply_projection_mutation_tx`
/// — the same cross-carrier overlap the retract/eviction paths guard, on a
/// path an earlier fix missed (Copilot/Codex review).
#[tokio::test]
async fn dm_publish_clearing_override_does_not_clobber_muc_projection_row_for_same_jid() {
    use crate::notification_settings_projection::{
        NotificationSettingsProjectionStore, NotificationSettingsSource,
    };
    use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let projection = NotificationSettingsProjectionStore::new(storage.database());
    let alice = jid("alice@example.com");
    let bob = jid("bob@example.com");

    // 1) XEP-0402 conference bookmark for bob@example.com with <never/>
    //    → an Xep0402Bookmarks-sourced projection row.
    storage
        .get_or_create_node(&alice, waddle_xmpp::xep::xep0402::PEP_NODE)
        .await
        .expect("bookmarks node");
    let muc_item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(conference_with_notify("never")),
    };
    storage
        .publish_item(
            &alice,
            waddle_xmpp::xep::xep0402::PEP_NODE,
            &muc_item,
            Some(&alice),
            false,
        )
        .await
        .expect("muc publish");
    assert_eq!(
        projection
            .get(&alice, &bob)
            .await
            .expect("get")
            .expect("muc row present")
            .source,
        NotificationSettingsSource::Xep0402Bookmarks
    );

    // 2) Publish an EMPTY <dm-bookmark/> (no <notify>) for the same id
    //    bob@example.com → derives Delete(source = WaddleDmBookmarks).
    //    The source-scoped delete must find no WaddleDmBookmarks row and
    //    leave the MUC row intact.
    storage
        .get_or_create_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("dm node");
    let empty_dm_item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(
            "<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0' />"
                .parse()
                .expect("valid empty dm-bookmark"),
        ),
    };
    storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &empty_dm_item,
            Some(&alice),
            false,
        )
        .await
        .expect("empty dm publish");

    let surviving = projection
        .get(&alice, &bob)
        .await
        .expect("get after empty dm publish")
        .expect("MUC-sourced row must survive the DM clear");
    assert_eq!(
        surviving.source,
        NotificationSettingsSource::Xep0402Bookmarks,
        "the source-scoped Delete must not clobber the MUC row"
    );
    assert_eq!(surviving.mode, waddle_xmpp::xep::NotificationLevel::Never);
}

/// A publisher that is NOT the node owner MUST be rejected with
/// `<forbidden/>` when targeting `urn:waddle:dm-bookmarks:0` (FIX 2,
/// security — paralleling `dnd_publish_from_non_owner_is_forbidden`).
/// DM notification settings are user-identity state; even if the owner
/// grants a peer a Publisher affiliation or opens the publish model, the
/// peer must not be able to write the owner's per-contact overrides. The
/// rejected publish writes no `pubsub_items` and no projection row.
#[tokio::test]
async fn dm_bookmark_publish_from_non_owner_is_forbidden() {
    use crate::notification_settings_projection::NotificationSettingsProjectionStore;
    use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let projection = NotificationSettingsProjectionStore::new(storage.database());
    let alice = jid("alice@example.com");
    let bob = jid("bob@example.com");
    let mallory = jid("mallory@example.com");

    storage
        .get_or_create_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("dm node");

    let item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(dm_bookmark_payload(
            "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>",
        )),
    };

    let err = storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &item,
            Some(&mallory),
            false,
        )
        .await
        .expect_err("non-owner DM publish must error");
    match &err {
        waddle_xmpp::XmppError::Stanza { condition, .. } => {
            assert!(
                format!("{condition:?}")
                    .to_lowercase()
                    .contains("forbidden"),
                "expected <forbidden/> condition, got: {condition:?}"
            );
        }
        other => panic!("expected Stanza forbidden error, got: {other:?}"),
    }

    // No pubsub item and no projection row may have been written.
    let stored = storage
        .get_items(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS, None, &[])
        .await
        .expect("get items");
    assert!(
        stored.is_empty(),
        "a forbidden DM publish must not leave a pubsub_items row"
    );
    assert!(
        projection
            .get(&alice, &bob)
            .await
            .expect("get projection")
            .is_none(),
        "a forbidden DM publish must not write a projection row"
    );

    // Sanity: the owner can still publish.
    storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &item,
            Some(&alice),
            false,
        )
        .await
        .expect("owner publish should succeed");
}

/// Deleting the DM-bookmarks node MUST clear its WaddleDmBookmarks
/// projection rows in the same tx while leaving an unrelated XEP-0402 MUC
/// row untouched (FIX 4, #720 adversarial review). Without the fix,
/// `delete_node_impl` only special-cased the bookmarks node, so deleting
/// the DM node orphaned every Direct projection row (stale suppression
/// with no wire way to clear it).
#[tokio::test]
async fn dm_bookmark_node_delete_clears_only_direct_projection_rows() {
    use crate::notification_settings_projection::{
        ConversationKind, NotificationSettingsProjection, NotificationSettingsProjectionStore,
        NotificationSettingsSource,
    };
    use waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS;

    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("storage");
    let projection = NotificationSettingsProjectionStore::new(storage.database());
    let alice = jid("alice@example.com");
    let bob = jid("bob@example.com");
    let room = jid("room@muc.example.com");

    // Seed a MUC (XEP-0402) projection row directly.
    projection
        .upsert(&NotificationSettingsProjection {
            owner_bare_jid: alice.clone(),
            conversation_jid: room.clone(),
            conversation_kind: ConversationKind::PrivateGroup,
            mode: waddle_xmpp::xep::NotificationLevel::Never,
            rich_payload_opt_in: false,
            source_version: 1,
            updated_at_ms: 1,
            source: NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: room.clone(),
        })
        .await
        .expect("seed muc row");

    // Publish a DM override so a Direct row exists.
    storage
        .get_or_create_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("dm node");
    let item = waddle_xmpp::pubsub::PubSubItem {
        id: Some("bob@example.com".to_string()),
        publisher: None,
        payload: Some(dm_bookmark_payload(
            "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>",
        )),
    };
    storage
        .publish_item(
            &alice,
            PEP_NODE_WADDLE_DM_BOOKMARKS,
            &item,
            Some(&alice),
            false,
        )
        .await
        .expect("dm publish");
    assert!(projection
        .get(&alice, &bob)
        .await
        .expect("dm row")
        .is_some());

    let deleted = storage
        .delete_node(&alice, PEP_NODE_WADDLE_DM_BOOKMARKS)
        .await
        .expect("delete dm node");
    assert!(deleted, "the DM node must be deleted");

    assert!(
        projection
            .get(&alice, &bob)
            .await
            .expect("dm row after delete")
            .is_none(),
        "deleting the DM node must clear its Direct projection rows"
    );
    assert!(
        projection
            .get(&alice, &room)
            .await
            .expect("muc row after delete")
            .is_some(),
        "deleting the DM node must NOT touch XEP-0402 MUC projection rows"
    );
}
