use jid::{BareJid, Jid};
use waddle_xmpp_core::pubsub::{Affiliation, SubscriptionState};

use crate::pubsub::node::NodeConfig;
use crate::pubsub::stanzas::PubSubItem;

use super::*;

#[test]
fn test_pubsub_node_new_pep() {
    let owner: BareJid = "user@example.com".parse().expect("valid jid");
    let node = PubSubNode::new_pep(owner.clone(), "test-node".to_string());

    assert_eq!(node.node_name, "test-node");
    assert_eq!(node.owner, owner);
    assert_eq!(node.config.max_items, 1);
}

#[test]
fn test_stored_item_to_pubsub_item() {
    let stored = StoredItem {
        id: "item-1".to_string(),
        payload_xml: Some("<test xmlns='test:ns'/>".to_string()),
        publisher: None,
        published_at: chrono::Utc::now(),
    };

    let pubsub_item = stored.to_pubsub_item();

    assert_eq!(pubsub_item.id, Some("item-1".to_string()));
    assert!(pubsub_item.payload.is_some());
}

#[tokio::test]
async fn test_in_memory_storage_get_or_create() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "user@example.com".parse().expect("valid jid");

    let (node, created) = storage
        .get_or_create_node(&owner, "test-node")
        .await
        .expect("should succeed");
    assert!(created);
    assert_eq!(node.node_name, "test-node");

    let (node2, created2) = storage
        .get_or_create_node(&owner, "test-node")
        .await
        .expect("should succeed");
    assert!(!created2);
    assert_eq!(node2.node_name, "test-node");
}

#[tokio::test]
async fn test_in_memory_storage_publish_and_get() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "user@example.com".parse().expect("valid jid");

    let item = PubSubItem::new(Some("item-1".to_string()), None);
    let result = storage
        .publish_item(&owner, "test-node", &item, Some(&owner), true)
        .await
        .expect("should succeed");

    assert_eq!(result.item_id, "item-1");
    assert!(result.node_created);

    let items = storage
        .get_items(&owner, "test-node", None, &[])
        .await
        .expect("should succeed");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "item-1");
}

#[tokio::test]
async fn test_in_memory_storage_max_items_enforced() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "user@example.com".parse().expect("valid jid");

    storage
        .get_or_create_node(&owner, "test-node")
        .await
        .expect("should succeed");

    for i in 1..=5 {
        let item = PubSubItem::new(Some(format!("item-{}", i)), None);
        storage
            .publish_item(&owner, "test-node", &item, None, false)
            .await
            .expect("should succeed");
    }

    let items = storage
        .get_items(&owner, "test-node", None, &[])
        .await
        .expect("should succeed");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "item-5");
}

#[tokio::test]
async fn test_in_memory_storage_retract() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "user@example.com".parse().expect("valid jid");

    storage
        .get_or_create_node(&owner, "test-node")
        .await
        .expect("should succeed");

    let mut config = NodeConfig::pep_default();
    config.max_items = 10;
    storage
        .update_node_config(&owner, "test-node", &config)
        .await
        .expect("should succeed");

    for i in 1..=3 {
        let item = PubSubItem::new(Some(format!("item-{}", i)), None);
        storage
            .publish_item(&owner, "test-node", &item, None, false)
            .await
            .expect("should succeed");
    }

    let retracted = storage
        .retract_item(&owner, "test-node", "item-2")
        .await
        .expect("should succeed");
    assert!(retracted);

    let items = storage
        .get_items(&owner, "test-node", None, &[])
        .await
        .expect("should succeed");

    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|i| i.id == "item-1"));
    assert!(items.iter().any(|i| i.id == "item-3"));
    assert!(!items.iter().any(|i| i.id == "item-2"));
}

#[tokio::test]
async fn test_in_memory_storage_delete_node() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "user@example.com".parse().expect("valid jid");

    let item = PubSubItem::new(Some("item-1".to_string()), None);
    storage
        .publish_item(&owner, "test-node", &item, None, true)
        .await
        .expect("should succeed");

    let deleted = storage
        .delete_node(&owner, "test-node")
        .await
        .expect("should succeed");
    assert!(deleted);

    let node = storage
        .get_node(&owner, "test-node")
        .await
        .expect("should succeed");
    assert!(node.is_none());

    let items = storage
        .get_items(&owner, "test-node", None, &[])
        .await
        .expect("should succeed");
    assert!(items.is_empty());
}

#[tokio::test]
async fn test_in_memory_storage_list_nodes() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "user@example.com".parse().expect("valid jid");
    let other: BareJid = "other@example.com".parse().expect("valid jid");

    storage
        .get_or_create_node(&owner, "node-1")
        .await
        .expect("should succeed");
    storage
        .get_or_create_node(&owner, "node-2")
        .await
        .expect("should succeed");
    storage
        .get_or_create_node(&other, "other-node")
        .await
        .expect("should succeed");

    let nodes = storage.list_nodes(&owner).await.expect("should succeed");

    assert_eq!(nodes.len(), 2);
    assert!(nodes.contains(&"node-1".to_string()));
    assert!(nodes.contains(&"node-2".to_string()));
    assert!(!nodes.contains(&"other-node".to_string()));
}

#[tokio::test]
async fn in_memory_subscribe_returns_unique_subids() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "u@x.com".parse().expect("bare jid");
    let alice: Jid = "alice@x.com".parse().expect("jid");

    let s1 = storage
        .subscribe(&owner, "node", &alice)
        .await
        .expect("sub");
    let s2 = storage
        .subscribe(&owner, "node", &alice)
        .await
        .expect("sub");
    assert_ne!(s1.subid, s2.subid);
    assert_eq!(s1.state, SubscriptionState::Subscribed);
}

#[tokio::test]
async fn in_memory_unsubscribe_with_subid_targets_one_row() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "u@x.com".parse().expect("bare jid");
    let alice: Jid = "alice@x.com".parse().expect("jid");

    let s1 = storage
        .subscribe(&owner, "node", &alice)
        .await
        .expect("sub");
    let _s2 = storage
        .subscribe(&owner, "node", &alice)
        .await
        .expect("sub");

    let removed = storage
        .unsubscribe(&owner, "node", &alice, Some(&s1.subid))
        .await
        .expect("unsubscribe");
    assert!(removed);

    let remaining = storage
        .list_node_subscriptions(&owner, "node")
        .await
        .expect("list");
    assert_eq!(remaining.len(), 1);
}

#[tokio::test]
async fn in_memory_set_affiliation_none_deletes_row() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "u@x.com".parse().expect("bare jid");
    let entity: BareJid = "bob@x.com".parse().expect("bare jid");

    let prev = storage
        .set_affiliation(&owner, "node", &entity, Affiliation::Outcast)
        .await
        .expect("set");
    assert_eq!(prev, Affiliation::None);
    assert_eq!(
        storage
            .get_affiliation(&owner, "node", &entity)
            .await
            .expect("get"),
        Affiliation::Outcast
    );

    let prev = storage
        .set_affiliation(&owner, "node", &entity, Affiliation::None)
        .await
        .expect("set");
    assert_eq!(prev, Affiliation::Outcast);
    assert_eq!(
        storage
            .get_affiliation(&owner, "node", &entity)
            .await
            .expect("get"),
        Affiliation::None
    );
}

#[tokio::test]
async fn in_memory_deliverable_subscribers_excludes_outcasts() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "u@x.com".parse().expect("bare jid");
    let alice: Jid = "alice@x.com".parse().expect("jid");
    let bob: Jid = "bob@x.com".parse().expect("jid");

    storage
        .subscribe(&owner, "node", &alice)
        .await
        .expect("sub");
    storage.subscribe(&owner, "node", &bob).await.expect("sub");

    let bob_bare: BareJid = "bob@x.com".parse().expect("bare jid");
    storage
        .set_affiliation(&owner, "node", &bob_bare, Affiliation::Outcast)
        .await
        .expect("set");

    let deliverable = storage
        .list_deliverable_subscribers(&owner, "node")
        .await
        .expect("list");
    assert_eq!(deliverable.len(), 1);
    assert_eq!(deliverable[0].subscriber.to_string(), "alice@x.com");
}

#[tokio::test]
async fn in_memory_purge_clears_items_keeps_node() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "u@x.com".parse().expect("bare jid");
    for i in 1..=3 {
        let item = PubSubItem::new(Some(format!("i{i}")), None);
        storage
            .publish_item(&owner, "n", &item, None, true)
            .await
            .expect("publish");
    }
    let _removed = storage.purge_node(&owner, "n").await.expect("purge");
    let items = storage
        .get_items(&owner, "n", None, &[])
        .await
        .expect("get");
    assert!(items.is_empty());
    assert!(storage.get_node(&owner, "n").await.expect("get").is_some());
}
