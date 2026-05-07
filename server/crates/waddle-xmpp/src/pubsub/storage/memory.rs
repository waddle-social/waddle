use async_trait::async_trait;
use jid::{BareJid, Jid};
use waddle_xmpp_core::pubsub::{Affiliation, SubId, Subscription, SubscriptionState};

use crate::pubsub::node::NodeConfig;
use crate::pubsub::stanzas::PubSubItem;
use crate::XmppError;

use super::{PubSubNode, PubSubStorage, PublishResult, StoredItem};

/// In-memory implementation of PubSub storage.
///
/// Uses DashMap for thread-safe concurrent access. Suitable for development
/// and single-node deployments. For production multi-node setups, consider
/// a database-backed implementation.
pub struct InMemoryPubSubStorage {
    /// (owner_bare_jid, node_name) -> PubSubNode
    nodes: dashmap::DashMap<(String, String), PubSubNode>,
    /// (owner_bare_jid, node_name) -> Vec<StoredItem>
    items: dashmap::DashMap<(String, String), Vec<StoredItem>>,
    /// (owner_bare_jid, node_name, subid) -> Subscription
    subscriptions: dashmap::DashMap<(String, String, String), Subscription>,
    /// (owner_bare_jid, node_name, entity_bare_jid) -> Affiliation
    affiliations: dashmap::DashMap<(String, String, String), Affiliation>,
}

impl Default for InMemoryPubSubStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryPubSubStorage {
    pub fn new() -> Self {
        Self {
            nodes: dashmap::DashMap::new(),
            items: dashmap::DashMap::new(),
            subscriptions: dashmap::DashMap::new(),
            affiliations: dashmap::DashMap::new(),
        }
    }

    fn key(owner: &BareJid, node_name: &str) -> (String, String) {
        (owner.to_string(), node_name.to_string())
    }

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

        if let Some(node) = self.nodes.get(&key) {
            return Ok((node.clone(), false));
        }

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

        let item_id = item.id.clone().unwrap_or_else(Self::generate_item_id);

        let stored_item = StoredItem {
            id: item_id.clone(),
            payload_xml: item.payload.as_ref().map(String::from),
            publisher: publisher.cloned(),
            published_at: chrono::Utc::now(),
        };

        let mut items = self.items.entry(key).or_default();

        if let Some(pos) = items.iter().position(|i| i.id == item_id) {
            items[pos] = stored_item;
        } else {
            items.push(stored_item);
        }

        let max_items = node.config.max_items as usize;
        if max_items > 0 && items.len() > max_items {
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

        let filtered: Vec<StoredItem> = if item_ids.is_empty() {
            items.clone()
        } else {
            items
                .iter()
                .filter(|i| item_ids.contains(&i.id))
                .cloned()
                .collect()
        };

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

    async fn find_node_for_item(
        &self,
        owner: &BareJid,
        item_id: &str,
    ) -> Result<Option<PubSubNode>, XmppError> {
        let owner_str = owner.to_string();
        for entry in self.nodes.iter() {
            if entry.key().0 != owner_str {
                continue;
            }
            let key = entry.key().clone();
            let node = entry.value().clone();
            if let Some(items) = self.items.get(&key) {
                if items.iter().any(|i| i.id == item_id) {
                    return Ok(Some(node));
                }
            }
        }
        Ok(None)
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

    async fn purge_node(&self, owner: &BareJid, node_name: &str) -> Result<u64, XmppError> {
        let key = Self::key(owner, node_name);
        let removed = match self.items.get_mut(&key) {
            Some(mut items) => {
                let n = items.len() as u64;
                items.clear();
                n
            }
            None => 0,
        };
        Ok(removed)
    }

    async fn subscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
    ) -> Result<Subscription, XmppError> {
        let subid = SubId::generate();
        let sub = Subscription {
            subid: subid.clone(),
            subscriber: subscriber.clone(),
            state: SubscriptionState::Subscribed,
            created_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        let key = (
            owner.to_string(),
            node_name.to_string(),
            subid.as_str().to_string(),
        );
        self.subscriptions.insert(key, sub.clone());
        Ok(sub)
    }

    async fn unsubscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
        subid: Option<&SubId>,
    ) -> Result<bool, XmppError> {
        let owner_str = owner.to_string();
        let node_str = node_name.to_string();
        let subscriber_str = subscriber.to_string();

        if let Some(subid) = subid {
            let key = (owner_str, node_str, subid.as_str().to_string());
            return Ok(self
                .subscriptions
                .remove_if(&key, |_, sub| sub.subscriber.to_string() == subscriber_str)
                .is_some());
        }

        let mut victim = None;
        for entry in self.subscriptions.iter() {
            let (k_owner, k_node, _) = entry.key();
            if k_owner == &owner_str
                && k_node == &node_str
                && entry.value().subscriber.to_string() == subscriber_str
            {
                victim = Some(entry.key().clone());
                break;
            }
        }
        Ok(victim.and_then(|k| self.subscriptions.remove(&k)).is_some())
    }

    async fn list_node_subscriptions(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError> {
        let owner_str = owner.to_string();
        let node_str = node_name.to_string();
        Ok(self
            .subscriptions
            .iter()
            .filter(|e| e.key().0 == owner_str && e.key().1 == node_str)
            .map(|e| e.value().clone())
            .collect())
    }

    async fn list_subscriber_subscriptions(
        &self,
        owner: &BareJid,
        subscriber: &Jid,
    ) -> Result<Vec<(String, Subscription)>, XmppError> {
        let owner_str = owner.to_string();
        let subscriber_str = subscriber.to_string();
        Ok(self
            .subscriptions
            .iter()
            .filter(|e| {
                e.key().0 == owner_str && e.value().subscriber.to_string() == subscriber_str
            })
            .map(|e| (e.key().1.clone(), e.value().clone()))
            .collect())
    }

    async fn get_subscription(
        &self,
        owner: &BareJid,
        node_name: &str,
        subid: &SubId,
    ) -> Result<Option<Subscription>, XmppError> {
        let key = (
            owner.to_string(),
            node_name.to_string(),
            subid.as_str().to_string(),
        );
        Ok(self.subscriptions.get(&key).map(|v| v.clone()))
    }

    async fn list_deliverable_subscribers(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError> {
        let owner_str = owner.to_string();
        let node_str = node_name.to_string();
        let mut out = Vec::new();
        for entry in self.subscriptions.iter() {
            if entry.key().0 != owner_str || entry.key().1 != node_str {
                continue;
            }
            let sub = entry.value();
            if sub.state != SubscriptionState::Subscribed {
                continue;
            }
            let entity_bare = sub.subscriber.to_bare();
            let aff_key = (owner_str.clone(), node_str.clone(), entity_bare.to_string());
            let outcast = self
                .affiliations
                .get(&aff_key)
                .map(|v| v.is_outcast())
                .unwrap_or(false);
            if outcast {
                continue;
            }
            out.push(sub.clone());
        }
        Ok(out)
    }

    async fn set_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
        affiliation: Affiliation,
    ) -> Result<Affiliation, XmppError> {
        let key = (owner.to_string(), node_name.to_string(), entity.to_string());
        if affiliation == Affiliation::None {
            return Ok(self
                .affiliations
                .remove(&key)
                .map(|(_, v)| v)
                .unwrap_or(Affiliation::None));
        }
        let prev = self.affiliations.insert(key, affiliation);
        Ok(prev.unwrap_or(Affiliation::None))
    }

    async fn get_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
    ) -> Result<Affiliation, XmppError> {
        let key = (owner.to_string(), node_name.to_string(), entity.to_string());
        Ok(self
            .affiliations
            .get(&key)
            .map(|v| *v)
            .unwrap_or(Affiliation::None))
    }

    async fn list_node_affiliations(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<(BareJid, Affiliation)>, XmppError> {
        let owner_str = owner.to_string();
        let node_str = node_name.to_string();
        let mut out = Vec::new();
        for entry in self.affiliations.iter() {
            if entry.key().0 == owner_str && entry.key().1 == node_str {
                let entity = entry
                    .key()
                    .2
                    .parse::<BareJid>()
                    .map_err(|e| XmppError::internal(e.to_string()))?;
                out.push((entity, *entry.value()));
            }
        }
        Ok(out)
    }

    async fn list_entity_affiliations(
        &self,
        owner: &BareJid,
        entity: &BareJid,
    ) -> Result<Vec<(String, Affiliation)>, XmppError> {
        let owner_str = owner.to_string();
        let entity_str = entity.to_string();
        Ok(self
            .affiliations
            .iter()
            .filter(|e| e.key().0 == owner_str && e.key().2 == entity_str)
            .map(|e| (e.key().1.clone(), *e.value()))
            .collect())
    }
}
