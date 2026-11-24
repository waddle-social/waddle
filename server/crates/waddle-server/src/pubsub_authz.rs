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
    let stored = storage.affiliation(target, node, entity).await?;
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
/// `is_pep` controls owner-derivation. This storage-only gate admits only
/// the owner under the Presence/Roster access models; PEP reads and
/// subscriptions that must honour the owner's roster go through
/// [`pep_read_decision`], which layers the roster lookup on this decision.
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
    // The XEP-0402 bookmarks node and the joined-spaces node are the
    // account's private data (XEP-0223): only the account reads them,
    // whatever access model or affiliation rows the node carries.
    if is_pep && is_owner_only_pep_node(node) {
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

/// The owner's roster standing of an entity, as the `presence` and
/// `roster` access models (XEP-0060 §4.5.1) judge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactStanding {
    /// The entity holds a presence subscription to the owner
    /// (roster subscription `from` or `both`).
    pub receives_owner_presence: bool,
    /// The roster groups the owner filed the entity under.
    pub groups: Vec<String>,
}

/// Outcome of the access-model gate for reading or subscribing to a PEP node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadDecision {
    Allowed,
    /// Refused without a model-specific condition (`forbidden`).
    Denied,
    /// `presence` model, no presence subscription (XEP-0060 §6.1.3.2).
    PresenceSubscriptionRequired,
    /// `roster` model, entity in none of the allowed groups (§6.1.3.3).
    NotInRosterGroup,
}

/// Pure presence/roster decision for a non-owner entity.
///
/// * `presence`: the entity MUST hold a presence subscription to the owner.
/// * `roster`: the entity MUST hold a presence subscription to the owner
///   (roster subscription `from` or `both`) AND be filed under at least one
///   group of `pubsub#roster_groups_allowed` (XEP-0060 §4.5.1, XEP-0163 §4).
///   With no allowed group configured no non-owner qualifies.
///
/// Other access models are not decided here and yield `Denied`.
pub fn contact_decision(
    node_cfg: &waddle_xmpp::pubsub::NodeConfig,
    standing: Option<&ContactStanding>,
) -> ReadDecision {
    match node_cfg.access_model {
        AccessModel::Presence => {
            if standing.is_some_and(|s| s.receives_owner_presence) {
                ReadDecision::Allowed
            } else {
                ReadDecision::PresenceSubscriptionRequired
            }
        }
        AccessModel::Roster => {
            if standing.is_some_and(|s| {
                s.receives_owner_presence
                    && s.groups
                        .iter()
                        .any(|group| node_cfg.roster_groups_allowed.contains(group))
            }) {
                ReadDecision::Allowed
            } else {
                ReadDecision::NotInRosterGroup
            }
        }
        _ => ReadDecision::Denied,
    }
}

/// The owner-side roster and blocklist the presence and roster access
/// models consult.
#[derive(Clone, Copy)]
pub struct ContactDirectory<'a> {
    roster: &'a crate::db::roster::DatabaseRosterStorage,
    blocking: &'a crate::db::blocking::DatabaseBlockingStorage,
}

impl<'a> ContactDirectory<'a> {
    pub fn new(pool: &'a crate::db::DatabasePool) -> Self {
        Self {
            roster: pool.roster_storage(),
            blocking: pool.blocking_storage(),
        }
    }

    /// The owner's roster contacts [`contact_decision`] admits to the node,
    /// for fan-out that walks the roster instead of asking per entity.
    /// Applies the same decision as [`pep_read_decision`], so notification
    /// recipients can never be wider than item readers.
    pub async fn allowed_contacts(
        &self,
        owner: &BareJid,
        node_cfg: &waddle_xmpp::pubsub::NodeConfig,
    ) -> Result<std::collections::HashSet<BareJid>, XmppError> {
        let rows = self
            .roster
            .get_roster(owner)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let mut allowed = std::collections::HashSet::new();
        for row in rows {
            let Ok(contact) = row.contact_jid.parse::<BareJid>() else {
                continue;
            };
            let standing = ContactStanding {
                receives_owner_presence: matches!(row.subscription.as_str(), "from" | "both"),
                groups: row.groups,
            };
            if contact_decision(node_cfg, Some(&standing)) == ReadDecision::Allowed {
                allowed.insert(contact);
            }
        }
        Ok(allowed)
    }

    /// Whether the owner has blocked `entity`, or `entity` has blocked the
    /// owner (XEP-0191; XEP-0163 §4: a PEP service MUST NOT notify a
    /// subscriber the user has blocked). Fails closed: an unreadable
    /// blocklist is an error the caller must treat as a refusal.
    pub async fn blocked_either_way(
        &self,
        owner: &BareJid,
        entity: &BareJid,
    ) -> Result<bool, XmppError> {
        let owner_jid = jid::Jid::from(owner.clone());
        let entity_jid = jid::Jid::from(entity.clone());
        let owner_list = self
            .blocking
            .load_blocklist(owner)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        if owner_list.contains_jid(&entity_jid) {
            return Ok(true);
        }
        let entity_list = self
            .blocking
            .load_blocklist(entity)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(entity_list.contains_jid(&owner_jid))
    }

    /// `None` when the entity is not in the owner's roster or is blocked by
    /// the owner (a blocked entity has no standing, XEP-0191).
    async fn standing(
        &self,
        owner: &BareJid,
        entity: &BareJid,
    ) -> Result<Option<ContactStanding>, XmppError> {
        let blocklist = self
            .blocking
            .load_blocklist(owner)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        if blocklist.contains_jid(&jid::Jid::from(entity.clone())) {
            return Ok(None);
        }
        let item = self
            .roster
            .get_roster_item(owner, entity)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(item.map(|item| ContactStanding {
            receives_owner_presence: matches!(item.subscription.as_str(), "from" | "both"),
            groups: item.groups,
        }))
    }
}

/// Access decision for reading items from, or subscribing to, a PEP node
/// (XEP-0060 §6.1.3, §6.5; XEP-0163 §4): the storage gate
/// ([`can_subscribe`]) first, then for the `presence` and `roster` models
/// the owner's roster standing of `entity`.
///
/// A missing node yields `Denied`; callers distinguish it themselves.
pub async fn pep_read_decision(
    storage: &Arc<dyn PubSubStorage>,
    contacts: &ContactDirectory<'_>,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
) -> Result<ReadDecision, XmppError> {
    // A blocked entity (either direction) is served no PEP node of the
    // owner, whatever its access model (XEP-0163 §4, XEP-0191).
    if !derive_pep_owner(target, entity) && contacts.blocked_either_way(target, entity).await? {
        return Ok(ReadDecision::Denied);
    }
    if can_subscribe(storage, target, node, entity, true).await? {
        return Ok(ReadDecision::Allowed);
    }
    if is_owner_only_pep_node(node) {
        return Ok(ReadDecision::Denied);
    }
    if storage
        .affiliation(target, node, entity)
        .await?
        .is_outcast()
    {
        return Ok(ReadDecision::Denied);
    }
    let Some(node_meta) = storage.get_node(target, node).await? else {
        return Ok(ReadDecision::Denied);
    };
    if !matches!(
        node_meta.config.access_model,
        AccessModel::Presence | AccessModel::Roster
    ) {
        return Ok(ReadDecision::Denied);
    }
    let standing = contacts.standing(target, entity).await?;
    Ok(contact_decision(&node_meta.config, standing.as_ref()))
}

/// Whether a PEP node is readable by its account alone, so that neither
/// its access model nor an affiliation extends reads, subscriptions or
/// event delivery to another entity.
pub fn is_owner_only_pep_node(node: &str) -> bool {
    node == waddle_xmpp::xep::xep0402::PEP_NODE
        || node == waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_JOINED_SPACES
}

/// Whether a PEP node is written and administered by its account alone,
/// so that neither an affiliation row (`owner`, `publisher`,
/// `publish-only`) nor its publish model extends publishing,
/// configuration, purging, deletion or affiliation and subscription
/// management to another entity.
///
/// The joined-spaces node is the account's private data (XEP-0223); its
/// affiliations set refuses such grants, and this keeps any row written
/// before or around that refusal from acting as one.
fn is_owner_managed_pep_node(node: &str) -> bool {
    node == waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_JOINED_SPACES
}

/// Whether `entity` is permitted to publish to a node (XEP-0060 §7.1.3).
pub async fn can_publish(
    storage: &Arc<dyn PubSubStorage>,
    target: &BareJid,
    node: &str,
    entity: &BareJid,
    is_pep: bool,
) -> Result<bool, XmppError> {
    if is_pep && is_owner_managed_pep_node(node) && !derive_pep_owner(target, entity) {
        return Ok(false);
    }
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
    if is_pep && is_owner_managed_pep_node(node) && !derive_pep_owner(target, entity) {
        return Ok(false);
    }
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

    #[test]
    fn presence_model_needs_presence_subscription() {
        let cfg = NodeConfig::pep_default();
        let from = ContactStanding {
            receives_owner_presence: true,
            groups: vec![],
        };
        let none = ContactStanding {
            receives_owner_presence: false,
            groups: vec![],
        };
        assert_eq!(contact_decision(&cfg, Some(&from)), ReadDecision::Allowed);
        assert_eq!(
            contact_decision(&cfg, Some(&none)),
            ReadDecision::PresenceSubscriptionRequired
        );
        assert_eq!(
            contact_decision(&cfg, None),
            ReadDecision::PresenceSubscriptionRequired
        );
    }

    #[test]
    fn roster_model_without_allowed_groups_refuses_every_non_owner() {
        let cfg = NodeConfig {
            access_model: AccessModel::Roster,
            ..NodeConfig::pep_default()
        };
        let contact = ContactStanding {
            receives_owner_presence: true,
            groups: vec!["Family".to_string()],
        };
        assert_eq!(
            contact_decision(&cfg, Some(&contact)),
            ReadDecision::NotInRosterGroup
        );
        assert_eq!(contact_decision(&cfg, None), ReadDecision::NotInRosterGroup);
    }

    #[test]
    fn roster_model_admits_a_contact_in_an_allowed_group() {
        let cfg = NodeConfig {
            access_model: AccessModel::Roster,
            roster_groups_allowed: vec!["Family".to_string()],
            ..NodeConfig::pep_default()
        };
        let contact = ContactStanding {
            receives_owner_presence: true,
            groups: vec!["Family".to_string()],
        };
        assert_eq!(
            contact_decision(&cfg, Some(&contact)),
            ReadDecision::Allowed
        );
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
