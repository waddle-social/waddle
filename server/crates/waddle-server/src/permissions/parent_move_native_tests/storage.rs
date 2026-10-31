//! Delegates every storage result to real PostgreSQL; only delays one completed stale-bookmark read.
use crate::pubsub::DatabasePubSubStorage;
use async_trait::async_trait;
use jid::{BareJid, Jid};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use waddle_xmpp::pubsub::{
    Affiliation, NodeConfig, PubSubItem, PubSubNode, PubSubStorage, PublicationError,
    PublicationNode, PublicationVersion, PublishResult, StoredItem, SubId, Subscription,
    VersionedPublishResult,
};
use waddle_xmpp::XmppError;

pub(super) struct CaptureBarrier {
    pub(super) captured: Notify,
    pub(super) release: Notify,
    pub(super) count: AtomicUsize,
    pub(super) frozen: Mutex<Option<Vec<PubSubItem>>>,
    pub(super) armed: AtomicBool,
}
impl CaptureBarrier {
    pub(super) fn new() -> Self {
        Self {
            captured: Notify::new(),
            release: Notify::new(),
            count: AtomicUsize::new(0),
            frozen: Mutex::new(None),
            armed: AtomicBool::new(false),
        }
    }
}
pub(super) struct CapturingStorage {
    pub(super) inner: Arc<DatabasePubSubStorage>,
    pub(super) barrier: Arc<CaptureBarrier>,
    pub(super) owner: BareJid,
    pub(super) node: String,
    pub(super) item_id: String,
}
#[async_trait]
impl PubSubStorage for CapturingStorage {
    async fn get_or_create_node(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<(PubSubNode, bool), XmppError> {
        self.inner.get_or_create_node(owner, node_name).await
    }
    async fn get_node(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Option<PubSubNode>, XmppError> {
        self.inner.get_node(owner, node_name).await
    }
    async fn delete_node(&self, owner: &BareJid, node_name: &str) -> Result<bool, XmppError> {
        self.inner.delete_node(owner, node_name).await
    }
    async fn publish_item(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: Option<&BareJid>,
        auto_create: bool,
    ) -> Result<PublishResult, XmppError> {
        self.inner
            .publish_item(owner, node_name, item, publisher, auto_create)
            .await
    }
    async fn publish_item_if_missing_or_publisher(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        auto_create: bool,
    ) -> Result<PublishResult, XmppError> {
        self.inner
            .publish_item_if_missing_or_publisher(owner, node_name, item, publisher, auto_create)
            .await
    }
    async fn publish_push_item_versioned(
        &self,
        service: &BareJid,
        publisher: &BareJid,
        node: &PublicationNode,
        item: &PubSubItem,
        version: PublicationVersion,
    ) -> Result<VersionedPublishResult, PublicationError> {
        self.inner
            .publish_push_item_versioned(service, publisher, node, item, version)
            .await
    }
    async fn get_items(
        &self,
        owner: &BareJid,
        node_name: &str,
        max_items: Option<u32>,
        item_ids: &[String],
    ) -> Result<Vec<StoredItem>, XmppError> {
        let rows = self
            .inner
            .get_items(owner, node_name, max_items, item_ids)
            .await?;
        if *owner == self.owner
            && node_name == self.node
            && max_items == Some(1)
            && item_ids == [self.item_id.clone()]
            && self.barrier.armed.swap(false, Ordering::SeqCst)
        {
            self.barrier.count.store(rows.len(), Ordering::SeqCst);
            *self.barrier.frozen.lock().expect("barrier lock") =
                Some(rows.iter().map(StoredItem::to_pubsub_item).collect());
            self.barrier.captured.notify_one();
            tokio::time::timeout(Duration::from_secs(20), self.barrier.release.notified())
                .await
                .expect("controller must release actual stale-bookmark read");
        }
        Ok(rows)
    }
    async fn retract_item(
        &self,
        owner: &BareJid,
        node_name: &str,
        item_id: &str,
    ) -> Result<bool, XmppError> {
        self.inner.retract_item(owner, node_name, item_id).await
    }
    async fn list_nodes(&self, owner: &BareJid) -> Result<Vec<String>, XmppError> {
        self.inner.list_nodes(owner).await
    }
    async fn find_node_for_item(
        &self,
        owner: &BareJid,
        item_id: &str,
    ) -> Result<Option<PubSubNode>, XmppError> {
        self.inner.find_node_for_item(owner, item_id).await
    }
    async fn list_node_names_for_item(
        &self,
        owner: &BareJid,
        item_id: &str,
    ) -> Result<Vec<String>, XmppError> {
        self.inner.list_node_names_for_item(owner, item_id).await
    }
    async fn update_node_config(
        &self,
        owner: &BareJid,
        node_name: &str,
        config: &NodeConfig,
    ) -> Result<(), XmppError> {
        self.inner
            .update_node_config(owner, node_name, config)
            .await
    }
    async fn mark_owner_configured(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<(), XmppError> {
        self.inner.mark_owner_configured(owner, node_name).await
    }
    async fn repair_legacy_avatar_node(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<bool, XmppError> {
        self.inner.repair_legacy_avatar_node(owner, node_name).await
    }
    async fn is_owner_configured(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<bool, XmppError> {
        self.inner.is_owner_configured(owner, node_name).await
    }
    async fn purge_node(&self, owner: &BareJid, node_name: &str) -> Result<u64, XmppError> {
        self.inner.purge_node(owner, node_name).await
    }
    async fn subscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
    ) -> Result<Subscription, XmppError> {
        self.inner.subscribe(owner, node_name, subscriber).await
    }
    async fn unsubscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
        subid: Option<&SubId>,
    ) -> Result<bool, XmppError> {
        self.inner
            .unsubscribe(owner, node_name, subscriber, subid)
            .await
    }
    async fn list_node_subscriptions(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError> {
        self.inner.list_node_subscriptions(owner, node_name).await
    }
    async fn list_subscriber_subscriptions(
        &self,
        owner: &BareJid,
        subscriber: &Jid,
    ) -> Result<Vec<(String, Subscription)>, XmppError> {
        self.inner
            .list_subscriber_subscriptions(owner, subscriber)
            .await
    }
    async fn get_subscription(
        &self,
        owner: &BareJid,
        node_name: &str,
        subid: &SubId,
    ) -> Result<Option<Subscription>, XmppError> {
        self.inner.get_subscription(owner, node_name, subid).await
    }
    async fn list_deliverable_subscribers(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError> {
        self.inner
            .list_deliverable_subscribers(owner, node_name)
            .await
    }
    async fn set_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
        affiliation: Affiliation,
    ) -> Result<Affiliation, XmppError> {
        self.inner
            .set_affiliation(owner, node_name, entity, affiliation)
            .await
    }
    async fn affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
    ) -> Result<Affiliation, XmppError> {
        self.inner.affiliation(owner, node_name, entity).await
    }
    async fn list_node_affiliations(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<(BareJid, Affiliation)>, XmppError> {
        self.inner.list_node_affiliations(owner, node_name).await
    }
    async fn list_entity_affiliations(
        &self,
        owner: &BareJid,
        entity: &BareJid,
    ) -> Result<Vec<(String, Affiliation)>, XmppError> {
        self.inner.list_entity_affiliations(owner, entity).await
    }
}
