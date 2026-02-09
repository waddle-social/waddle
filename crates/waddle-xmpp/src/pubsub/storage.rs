//! PubSub storage trait and types.
//!
//! Defines the storage interface for PubSub nodes and items.

use async_trait::async_trait;
use jid::BareJid;

use super::node::NodeConfig;
use super::stanzas::PubSubItem;
use crate::XmppError;

/// Stored representation of a PubSub node.
#[derive(Debug, Clone)]
pub struct PubSubNode {
    /// Unique node identifier (within an owner's namespace).
    pub node_name: String,
    /// The bare JID of the node owner.
    pub owner: BareJid,
    /// Node configuration.
    pub config: NodeConfig,
    /// When the node was created.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl PubSubNode {
    /// Create a new PubSub node with default PEP configuration.
    pub fn new_pep(owner: BareJid, node_name: String) -> Self {
        Self {
            node_name,
            owner,
            config: NodeConfig::pep_default(),
            created_at: chrono::Utc::now(),
        }
    }

    /// Create a new PubSub node with custom configuration.
    pub fn new(owner: BareJid, node_name: String, config: NodeConfig) -> Self {
        Self {
            node_name,
            owner,
            config,
            created_at: chrono::Utc::now(),
        }
    }
}

/// Stored representation of a PubSub item.
#[derive(Debug, Clone)]
pub struct StoredItem {
    /// Item ID.
    pub id: String,
    /// The item payload as XML string.
    pub payload_xml: Option<String>,
    /// Publisher's JID.
    pub publisher: Option<BareJid>,
    /// When the item was published.
    pub published_at: chrono::DateTime<chrono::Utc>,
}

impl StoredItem {
    /// Convert to a PubSubItem for responses.
    pub fn to_pubsub_item(&self) -> PubSubItem {
        let payload = self.payload_xml.as_ref().and_then(|xml| xml.parse().ok());

        PubSubItem {
            id: Some(self.id.clone()),
            payload,
        }
    }
}

/// Result of a publish operation.
#[derive(Debug)]
pub struct PublishResult {
    /// The assigned item ID (may be generated if not provided).
    pub item_id: String,
    /// Whether a new node was created (auto-create).
    pub node_created: bool,
}

/// Storage trait for PubSub nodes and items.
///
/// Implementations of this trait provide persistent storage for PubSub data.
/// The trait uses async methods to support both in-memory and database backends.
#[async_trait]
pub trait PubSubStorage: Send + Sync + 'static {
    /// Get or create a node for the given owner.
    ///
    /// If the node exists, returns it. Otherwise, creates a new node with
    /// default PEP configuration and returns it.
    ///
    /// This implements PEP auto-create behavior (XEP-0163).
    async fn get_or_create_node(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<(PubSubNode, bool), XmppError>;

    /// Get a node without creating it.
    ///
    /// Returns None if the node doesn't exist.
    async fn get_node(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Option<PubSubNode>, XmppError>;

    /// Delete a node and all its items.
    ///
    /// Returns true if the node was deleted, false if it didn't exist.
    async fn delete_node(&self, owner: &BareJid, node_name: &str) -> Result<bool, XmppError>;

    /// Publish an item to a node.
    ///
    /// If the node doesn't exist and auto_create is true, creates it.
    /// If item.id is None, generates a unique ID.
    ///
    /// Returns the assigned item ID and whether a new node was created.
    async fn publish_item(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: Option<&BareJid>,
        auto_create: bool,
    ) -> Result<PublishResult, XmppError>;

    /// Get items from a node.
    ///
    /// If item_ids is empty, returns all items (up to max_items if specified).
    /// If item_ids is provided, returns only those items.
    async fn get_items(
        &self,
        owner: &BareJid,
        node_name: &str,
        max_items: Option<u32>,
        item_ids: &[String],
    ) -> Result<Vec<StoredItem>, XmppError>;

    /// Retract (delete) an item from a node.
    ///
    /// Returns true if the item was deleted, false if it didn't exist.
    async fn retract_item(
        &self,
        owner: &BareJid,
        node_name: &str,
        item_id: &str,
    ) -> Result<bool, XmppError>;

    /// List all nodes owned by a JID.
    async fn list_nodes(&self, owner: &BareJid) -> Result<Vec<String>, XmppError>;

    /// Update node configuration.
    async fn update_node_config(
        &self,
        owner: &BareJid,
        node_name: &str,
        config: &NodeConfig,
    ) -> Result<(), XmppError>;
}

/// In-memory implementation of PubSub storage.
///
/// Uses DashMap for thread-safe concurrent access. Suitable for development
/// and single-node deployments. For production multi-node setups, consider
/// a database-backed implementation.
pub struct InMemoryPubSubStorage {
    /// Map of (owner_bare_jid, node_name) -> PubSubNode
    nodes: dashmap::DashMap<(String, String), PubSubNode>,
    /// Map of (owner_bare_jid, node_name) -> Vec<StoredItem>
    items: dashmap::DashMap<(String, String), Vec<StoredItem>>,
}

impl Default for InMemoryPubSubStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryPubSubStorage {
    /// Create a new in-memory PubSub storage.
    pub fn new() -> Self {
        Self {
            nodes: dashmap::DashMap::new(),
            items: dashmap::DashMap::new(),
        }
    }

    /// Create a storage key from owner and node name.
    fn key(owner: &BareJid, node_name: &str) -> (String, String) {
        (owner.to_string(), node_name.to_string())
    }

    /// Generate a unique item ID.
    fn generate_item_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

#[async_trait]
impl PubSubStorage for InMemoryPubSubStorage {
    async fn get_or_create_node(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<(PubSubNode, bool), XmppError> {
        let key = Self::key(owner, node_name);

        // Check if node exists
        if let Some(node) = self.nodes.get(&key) {
            return Ok((node.clone(), false));
        }

        // Create new node with PEP defaults
        let node = PubSubNode::new_pep(owner.clone(), node_name.to_string());
        self.nodes.insert(key.clone(), node.clone());
        self.items.insert(key, Vec::new());

        Ok((node, true))
    }

    async fn get_node(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Option<PubSubNode>, XmppError> {
        let key = Self::key(owner, node_name);
        Ok(self.nodes.get(&key).map(|n| n.clone()))
    }

    async fn delete_node(&self, owner: &BareJid, node_name: &str) -> Result<bool, XmppError> {
        let key = Self::key(owner, node_name);

        let node_existed = self.nodes.remove(&key).is_some();
        self.items.remove(&key);

        Ok(node_existed)
    }

    async fn publish_item(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: Option<&BareJid>,
        auto_create: bool,
    ) -> Result<PublishResult, XmppError> {
        let key = Self::key(owner, node_name);

        // Check if node exists, auto-create if needed
        let (node, node_created) = if let Some(node) = self.nodes.get(&key) {
            (node.clone(), false)
        } else if auto_create {
            let node = PubSubNode::new_pep(owner.clone(), node_name.to_string());
            self.nodes.insert(key.clone(), node.clone());
            self.items.insert(key.clone(), Vec::new());
            (node, true)
        } else {
            return Err(XmppError::item_not_found(Some(format!(
                "Node '{}' does not exist",
                node_name
            ))));
        };

        // Generate or use provided item ID
        let item_id = item.id.clone().unwrap_or_else(Self::generate_item_id);

        // Create stored item
        let stored_item = StoredItem {
            id: item_id.clone(),
            payload_xml: item.payload.as_ref().map(|e| String::from(e)),
            publisher: publisher.cloned(),
            published_at: chrono::Utc::now(),
        };

        // Store the item
        let mut items = self.items.entry(key).or_default();

        // Check if item with same ID exists (replace it)
        if let Some(pos) = items.iter().position(|i| i.id == item_id) {
            items[pos] = stored_item;
        } else {
            items.push(stored_item);
        }

        // Enforce max_items limit
        let max_items = node.config.max_items as usize;
        if max_items > 0 && items.len() > max_items {
            // Remove oldest items (items at the beginning)
            let excess = items.len() - max_items;
            items.drain(0..excess);
        }

        Ok(PublishResult {
            item_id,
            node_created,
        })
    }

    async fn get_items(
        &self,
        owner: &BareJid,
        node_name: &str,
        max_items: Option<u32>,
        item_ids: &[String],
    ) -> Result<Vec<StoredItem>, XmppError> {
        let key = Self::key(owner, node_name);

        let items = match self.items.get(&key) {
            Some(items) => items,
            None => return Ok(Vec::new()),
        };

        // Filter by item IDs if specified
        let filtered: Vec<StoredItem> = if item_ids.is_empty() {
            items.clone()
        } else {
            items
                .iter()
                .filter(|i| item_ids.contains(&i.id))
                .cloned()
                .collect()
        };

        // Apply max_items limit (return most recent)
        let result = if let Some(max) = max_items {
            let max = max as usize;
            if filtered.len() > max {
                filtered[filtered.len() - max..].to_vec()
            } else {
                filtered
            }
        } else {
            filtered
        };

        Ok(result)
    }

    async fn retract_item(
        &self,
        owner: &BareJid,
        node_name: &str,
        item_id: &str,
    ) -> Result<bool, XmppError> {
        let key = Self::key(owner, node_name);

        let mut items = match self.items.get_mut(&key) {
            Some(items) => items,
            None => return Ok(false),
        };

        let original_len = items.len();
        items.retain(|i| i.id != item_id);

        Ok(items.len() < original_len)
    }

    async fn list_nodes(&self, owner: &BareJid) -> Result<Vec<String>, XmppError> {
        let owner_str = owner.to_string();
        let nodes: Vec<String> = self
            .nodes
            .iter()
            .filter(|entry| entry.key().0 == owner_str)
            .map(|entry| entry.value().node_name.clone())
            .collect();

        Ok(nodes)
    }

    async fn update_node_config(
        &self,
        owner: &BareJid,
        node_name: &str,
        config: &NodeConfig,
    ) -> Result<(), XmppError> {
        let key = Self::key(owner, node_name);

        let mut node = self.nodes.get_mut(&key).ok_or_else(|| {
            XmppError::item_not_found(Some(format!("Node '{}' does not exist", node_name)))
        })?;

        node.config = config.clone();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
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

        // First call creates the node
        let (node, created) = storage
            .get_or_create_node(&owner, "test-node")
            .await
            .expect("should succeed");
        assert!(created);
        assert_eq!(node.node_name, "test-node");

        // Second call returns existing node
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

        // Publish an item with auto-create
        let item = PubSubItem::new(Some("item-1".to_string()), None);
        let result = storage
            .publish_item(&owner, "test-node", &item, Some(&owner), true)
            .await
            .expect("should succeed");

        assert_eq!(result.item_id, "item-1");
        assert!(result.node_created);

        // Get the item back
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

        // Create node (PEP default has max_items=1)
        storage
            .get_or_create_node(&owner, "test-node")
            .await
            .expect("should succeed");

        // Publish multiple items
        for i in 1..=5 {
            let item = PubSubItem::new(Some(format!("item-{}", i)), None);
            storage
                .publish_item(&owner, "test-node", &item, None, false)
                .await
                .expect("should succeed");
        }

        // Only the last item should remain (max_items=1)
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

        // Create node with higher max_items for this test
        let (_, _) = storage
            .get_or_create_node(&owner, "test-node")
            .await
            .expect("should succeed");

        // Update config to allow more items
        let mut config = NodeConfig::pep_default();
        config.max_items = 10;
        storage
            .update_node_config(&owner, "test-node", &config)
            .await
            .expect("should succeed");

        // Publish items
        for i in 1..=3 {
            let item = PubSubItem::new(Some(format!("item-{}", i)), None);
            storage
                .publish_item(&owner, "test-node", &item, None, false)
                .await
                .expect("should succeed");
        }

        // Retract item-2
        let retracted = storage
            .retract_item(&owner, "test-node", "item-2")
            .await
            .expect("should succeed");
        assert!(retracted);

        // Check remaining items
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

        // Create and populate node
        let item = PubSubItem::new(Some("item-1".to_string()), None);
        storage
            .publish_item(&owner, "test-node", &item, None, true)
            .await
            .expect("should succeed");

        // Delete node
        let deleted = storage
            .delete_node(&owner, "test-node")
            .await
            .expect("should succeed");
        assert!(deleted);

        // Verify node is gone
        let node = storage
            .get_node(&owner, "test-node")
            .await
            .expect("should succeed");
        assert!(node.is_none());

        // Verify items are gone
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

        // Create nodes for user
        storage
            .get_or_create_node(&owner, "node-1")
            .await
            .expect("should succeed");
        storage
            .get_or_create_node(&owner, "node-2")
            .await
            .expect("should succeed");

        // Create node for other user
        storage
            .get_or_create_node(&other, "other-node")
            .await
            .expect("should succeed");

        // List user's nodes
        let nodes = storage.list_nodes(&owner).await.expect("should succeed");

        assert_eq!(nodes.len(), 2);
        assert!(nodes.contains(&"node-1".to_string()));
        assert!(nodes.contains(&"node-2".to_string()));
        assert!(!nodes.contains(&"other-node".to_string()));
    }
}
