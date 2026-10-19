//! XEP-0060 / XEP-0163 authorization layered on top of `PubSubStorage` data
//! primitives. Storage knows nothing about XMPP semantics; this module knows
//! about access models, owner derivation, and outcast enforcement.

use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp::pubsub::PubSubStorage;
use waddle_xmpp::pubsub::{AccessModel, Affiliation, PublishModel};
use waddle_xmpp::XmppError;

/// Owner-derivation rule for PEP nodes (XEP-0163 §1).
///
/// For PEP, the node owner is the bare JID matching the target JID. For
/// non-PEP (Spaces, MUC#user) nodes, owner status is established by an
/// explicit affiliation row.
pub fn derive_pep_owner(target: &BareJid, entity: &BareJid) -> bool {
    target == entity
}

/// Resolve the effective affiliation: explicit row, falling back to derived
/// owner for PEP nodes (target_jid is the node owner / namespace owner —
/// for PEP it's the user JID hosting the node tree).
pub async fn effective_affiliation(
    storage: &Arc<dyn PubSubStorage>,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
    is_pep: bool,
) -> Result<Affiliation, XmppError> {
    let stored = storage.get_affiliation(target, node, entity).await?;
    if stored != Affiliation::None {
        return Ok(stored);
    }
    if is_pep && derive_pep_owner(target, entity) {
        return Ok(Affiliation::Owner);
    }
    Ok(Affiliation::None)
}

/// Whether `entity` is permitted to subscribe to a node (XEP-0060 §6.1).
///
/// `is_pep` controls owner-derivation. Presence/Roster access models defer
/// to roster+presence integration which is out of scope for the durability
/// PR; those models permit only the owner until that integration ships.
pub async fn can_subscribe(
    storage: &Arc<dyn PubSubStorage>,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
    is_pep: bool,
) -> Result<bool, XmppError> {
    let aff = effective_affiliation(storage, target, node, entity, is_pep).await?;
    if aff.is_outcast() {
        return Ok(false);
    }
    let Some(node_meta) = storage.get_node(target, node).await? else {
        return Ok(false);
    };
    Ok(can_subscribe_with_config(
        &node_meta.config,
        aff,
        target,
        node,
        entity,
        is_pep,
    ))
}

/// Access-model arm of [`can_subscribe`] against an already-loaded
/// node config. Callers that hold the config (the publish fan-out gate
/// fetches it once per publish) use this to avoid a redundant
/// `get_node` round-trip per entity; `can_subscribe` itself is the
/// fetching wrapper, so the decision logic cannot diverge between the
/// subscribe/read gate and the fan-out entitlement.
pub fn can_subscribe_with_config(
    node_cfg: &waddle_xmpp::pubsub::NodeConfig,
    aff: Affiliation,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
    is_pep: bool,
) -> bool {
    if aff.is_outcast() {
        return false;
    }
    if is_pep && node == waddle_xmpp::xep::xep0402::PEP_NODE {
        return derive_pep_owner(target, entity);
    }
    match node_cfg.access_model {
        AccessModel::Open => true,
        AccessModel::Whitelist => matches!(
            aff,
            Affiliation::Owner | Affiliation::Publisher | Affiliation::Member
        ),
        AccessModel::Presence | AccessModel::Roster => {
            matches!(aff, Affiliation::Owner) || (is_pep && derive_pep_owner(target, entity))
        }
        AccessModel::Authorize => matches!(aff, Affiliation::Owner),
    }
}

/// Whether `entity` is permitted to publish to a node (XEP-0060 §7.1.3).
pub async fn can_publish(
    storage: &Arc<dyn PubSubStorage>,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
    is_pep: bool,
) -> Result<bool, XmppError> {
    let aff = effective_affiliation(storage, target, node, entity, is_pep).await?;
    if aff.is_outcast() {
        return Ok(false);
    }
    let Some(node_meta) = storage.get_node(target, node).await? else {
        return Ok(false);
    };
    if matches!(aff, Affiliation::Owner) {
        return Ok(true);
    }
    match node_meta.config.publish_model {
        PublishModel::Open => Ok(true),
        PublishModel::Publishers => Ok(aff.can_publish_default()),
        PublishModel::Subscribers => {
            // Only a subscription to the *requested* node grants publish rights
            // (XEP-0060 §7.1.3). Subscriptions to other nodes under the same
            // owner must not be treated as eligible.
            let has_sub = storage
                .list_subscriber_subscriptions(target, &jid::Jid::from(entity.clone()))
                .await?
                .iter()
                .any(|(n, _)| n == node);
            Ok(has_sub || aff.can_publish_default())
        }
    }
}

/// Whether `entity` is permitted to configure or delete a node (owner only).
pub async fn can_administer(
    storage: &Arc<dyn PubSubStorage>,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
    is_pep: bool,
) -> Result<bool, XmppError> {
    let aff = effective_affiliation(storage, target, node, entity, is_pep).await?;
    Ok(matches!(aff, Affiliation::Owner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::pubsub::InMemoryPubSubStorage;
    use waddle_xmpp::pubsub::NodeConfig;

    fn jid(s: &str) -> BareJid {
        s.parse().expect("bare jid")
    }

    #[tokio::test]
    async fn pep_owner_is_self() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        storage
            .get_or_create_node(&alice, "urn:xmpp:bookmarks:1")
            .await
            .expect("node");

        let aff = effective_affiliation(&storage, &alice, "urn:xmpp:bookmarks:1", &alice, true)
            .await
            .expect("aff");
        assert_eq!(aff, Affiliation::Owner);
    }

    #[tokio::test]
    async fn explicit_owner_overrides_derived() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        let bob = jid("bob@x.com");
        storage.get_or_create_node(&alice, "n").await.expect("node");
        storage
            .set_affiliation(&alice, "n", &bob, Affiliation::Owner)
            .await
            .expect("set");

        let aff = effective_affiliation(&storage, &alice, "n", &bob, true)
            .await
            .expect("aff");
        assert_eq!(aff, Affiliation::Owner);
    }

    #[tokio::test]
    async fn outcast_cannot_subscribe_to_open_node() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        let bob = jid("bob@x.com");
        storage.get_or_create_node(&alice, "n").await.expect("node");
        storage
            .update_node_config(&alice, "n", &NodeConfig::public())
            .await
            .expect("config");
        storage
            .set_affiliation(&alice, "n", &bob, Affiliation::Outcast)
            .await
            .expect("set");

        assert!(!can_subscribe(&storage, &alice, "n", &bob, false)
            .await
            .expect("can_subscribe"));
    }

    #[tokio::test]
    async fn whitelist_denies_random_subscriber() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        let bob = jid("bob@x.com");
        storage.get_or_create_node(&alice, "n").await.expect("node");
        storage
            .update_node_config(&alice, "n", &NodeConfig::whitelist())
            .await
            .expect("config");

        assert!(!can_subscribe(&storage, &alice, "n", &bob, false)
            .await
            .expect("can_subscribe"));
        storage
            .set_affiliation(&alice, "n", &bob, Affiliation::Member)
            .await
            .expect("set");
        assert!(can_subscribe(&storage, &alice, "n", &bob, false)
            .await
            .expect("can_subscribe"));
    }

    #[tokio::test]
    async fn pep_bookmarks_are_owner_only_even_if_stored_config_is_open() {
        let concrete = Arc::new(
            crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
                .await
                .expect("storage"),
        );
        let storage: Arc<dyn PubSubStorage> = concrete.clone();
        let alice = jid("alice@x.com");
        let bob = jid("bob@x.com");
        storage
            .get_or_create_node(&alice, waddle_xmpp::xep::xep0402::PEP_NODE)
            .await
            .expect("node");
        storage
            .set_affiliation(
                &alice,
                waddle_xmpp::xep::xep0402::PEP_NODE,
                &bob,
                Affiliation::Member,
            )
            .await
            .expect("affiliation");

        let db = concrete.database();
        let conn = db.guard().await.expect("db");
        conn.execute(
            "UPDATE pubsub_nodes SET access_model = ? WHERE owner_jid = ? AND node_name = ?",
            crate::db_params![
                AccessModel::Open.to_string(),
                alice.to_string(),
                waddle_xmpp::xep::xep0402::PEP_NODE,
            ],
        )
        .await
        .expect("force open config");
        drop(conn);

        assert!(
            can_subscribe(
                &storage,
                &alice,
                waddle_xmpp::xep::xep0402::PEP_NODE,
                &alice,
                true,
            )
            .await
            .expect("owner can_subscribe"),
            "bookmark owner should retain access to their private PEP node"
        );
        assert!(
            !can_subscribe(
                &storage,
                &alice,
                waddle_xmpp::xep::xep0402::PEP_NODE,
                &bob,
                true,
            )
            .await
            .expect("bob can_subscribe"),
            "non-owner affiliation and open config must not grant bookmark reads"
        );
    }

    #[tokio::test]
    async fn pep_owner_can_publish() {
        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        storage.get_or_create_node(&alice, "n").await.expect("node");
        assert!(can_publish(&storage, &alice, "n", &alice, true)
            .await
            .expect("can_publish"));
    }

    #[tokio::test]
    async fn subscribers_model_scoped_to_requested_node() {
        use waddle_xmpp::pubsub::PublishModel;

        let storage: Arc<dyn PubSubStorage> = Arc::new(InMemoryPubSubStorage::new());
        let alice = jid("alice@x.com");
        let bob = jid("bob@x.com");

        // Create two nodes for alice.
        storage
            .get_or_create_node(&alice, "node-a")
            .await
            .expect("node-a");
        storage
            .get_or_create_node(&alice, "node-b")
            .await
            .expect("node-b");

        // Set both to publish_model = Subscribers.
        let cfg = NodeConfig {
            publish_model: PublishModel::Subscribers,
            ..NodeConfig::default()
        };
        storage
            .update_node_config(&alice, "node-a", &cfg)
            .await
            .expect("config node-a");
        storage
            .update_node_config(&alice, "node-b", &cfg)
            .await
            .expect("config node-b");

        // Bob subscribes only to node-a.
        storage
            .subscribe(&alice, "node-a", &jid::Jid::from(bob.clone()))
            .await
            .expect("subscribe");

        // Bob may publish to node-a (has subscription there).
        assert!(
            can_publish(&storage, &alice, "node-a", &bob, false)
                .await
                .expect("can_publish node-a"),
            "subscriber on node-a should be allowed to publish to node-a"
        );

        // Bob must NOT publish to node-b (subscription is on a different node).
        assert!(
            !can_publish(&storage, &alice, "node-b", &bob, false)
                .await
                .expect("can_publish node-b"),
            "subscription on node-a must not grant publish access to node-b"
        );
    }
}
