use async_trait::async_trait;
use jid::{BareJid, Jid};
use waddle_xmpp_core::pubsub::{Affiliation, SubId, Subscription};

use crate::pubsub::node::NodeConfig;
use crate::pubsub::stanzas::PubSubItem;
use crate::XmppError;

use super::{PubSubNode, PublishResult, StoredItem};

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

    /// Publish an item only when the item id is unused or the existing
    /// stored item was published by the same bare JID.
    ///
    /// Open community nodes use this to prevent two members from clobbering
    /// each other's client-chosen item ids while still allowing a publisher
    /// to update their own item. Implementations must enforce this in the
    /// same critical section or transaction as the write.
    async fn publish_item_if_missing_or_publisher(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: &BareJid,
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

    /// Find the first node owned by `owner` that contains an item with the
    /// given `item_id`, returning both the node name and the node metadata.
    ///
    /// Returns `Ok(None)` when no node contains the item.
    ///
    /// SQL-backed implementations should use an indexed join, e.g.:
    /// `SELECT nodes.* FROM nodes JOIN items ON nodes.id = items.node_id
    ///  WHERE nodes.owner = ? AND items.id = ?`
    /// to avoid a full table scan.
    async fn find_node_for_item(
        &self,
        owner: &BareJid,
        item_id: &str,
    ) -> Result<Option<PubSubNode>, XmppError>;

    /// List the names of every node owned by `owner` that contains an item
    /// with the given `item_id`. Distinct from `find_node_for_item` (which
    /// returns at most one): when an item legitimately or accidentally lives
    /// in multiple nodes, this surfaces the full set so callers can enforce
    /// single-membership invariants (e.g., XEP-0503 channel→space pinning).
    ///
    /// Order of returned names is unspecified.
    async fn list_node_names_for_item(
        &self,
        owner: &BareJid,
        item_id: &str,
    ) -> Result<Vec<String>, XmppError>;

    /// Update node configuration.
    async fn update_node_config(
        &self,
        owner: &BareJid,
        node_name: &str,
        config: &NodeConfig,
    ) -> Result<(), XmppError>;

    /// Purge all items from a node without deleting the node (XEP-0060 §8.5).
    /// Returns the number of items removed.
    async fn purge_node(&self, owner: &BareJid, node_name: &str) -> Result<u64, XmppError>;

    /// Create a new subscription. Always inserts a new row (multi-sub-per-jid
    /// allowed by XEP-0060 §6.1). Returns the generated subid + state.
    async fn subscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
    ) -> Result<Subscription, XmppError>;

    /// Remove a subscription. If `subid` is `Some`, target that exact row;
    /// if `None`, the caller must have already established that there is
    /// exactly one subscription for `subscriber` (see XEP-0060 §6.2.3.2).
    /// Returns true if a row was deleted.
    async fn unsubscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
        subid: Option<&SubId>,
    ) -> Result<bool, XmppError>;

    /// List all subscriptions for a node.
    async fn list_node_subscriptions(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError>;

    /// List all subscriptions held by a specific subscriber across all nodes
    /// owned by `owner`. Used to answer `<subscriptions/>` requests.
    async fn list_subscriber_subscriptions(
        &self,
        owner: &BareJid,
        subscriber: &Jid,
    ) -> Result<Vec<(String, Subscription)>, XmppError>;

    /// Look up a single subscription by `(owner, node, subid)`.
    async fn get_subscription(
        &self,
        owner: &BareJid,
        node_name: &str,
        subid: &SubId,
    ) -> Result<Option<Subscription>, XmppError>;

    /// Hot-path query for publish fan-out. Returns subscribers with state
    /// `Subscribed` whose entity is *not* `Outcast`. The exact return
    /// semantics: each row is a `Subscription`, already filtered.
    async fn list_deliverable_subscribers(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError>;

    /// Set or remove an affiliation. Setting `Affiliation::None` deletes
    /// the row. Returns the previous affiliation (`Affiliation::None` if
    /// no row existed).
    async fn set_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
        affiliation: Affiliation,
    ) -> Result<Affiliation, XmppError>;

    /// Read the explicit affiliation row for `(owner, node, entity)`.
    /// Returns `Affiliation::None` if no row exists. Owner-derivation for
    /// PEP nodes happens in `pubsub_authz`, *not* here.
    async fn get_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
    ) -> Result<Affiliation, XmppError>;

    /// List all explicit affiliation rows for a node.
    async fn list_node_affiliations(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<(BareJid, Affiliation)>, XmppError>;

    /// List all explicit affiliation rows held by a single entity across
    /// all nodes owned by `owner`.
    async fn list_entity_affiliations(
        &self,
        owner: &BareJid,
        entity: &BareJid,
    ) -> Result<Vec<(String, Affiliation)>, XmppError>;
}
