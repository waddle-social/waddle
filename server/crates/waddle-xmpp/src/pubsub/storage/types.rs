use jid::BareJid;

use crate::pubsub::node::NodeConfig;
use crate::pubsub::stanzas::PubSubItem;

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
            publisher: self.publisher.clone(),
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
