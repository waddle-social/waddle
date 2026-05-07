use std::path::PathBuf;

use super::*;

fn jid(value: &str) -> BareJid {
    value.parse().expect("valid JID")
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
            payload: None,
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
            payload: None,
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
    assert!(
        storage
            .get_items(&owner, "n", None, &[])
            .await
            .expect("items")
            .is_empty()
    );
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
