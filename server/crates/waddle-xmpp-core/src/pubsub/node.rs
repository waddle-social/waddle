//! Shared PubSub node configuration primitives.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// Bounded default for XEP-0402 bookmark PEP nodes.
///
/// XEP-0402 needs many persistent bookmark items, but an unbounded node makes
/// authenticated storage growth trivial. This is deliberately much larger than
/// normal client bookmark counts while still finite.
pub const PEP_BOOKMARK_MAX_ITEMS: u32 = 1024;

/// Access model for a PubSub node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AccessModel {
    /// Anyone can subscribe to and retrieve items.
    Open,
    /// Only entities in the owner's roster with a subscription can access.
    #[default]
    Presence,
    /// Only entities in specific roster groups can access.
    Roster,
    /// Only explicitly whitelisted JIDs can access.
    Whitelist,
    /// Only the node owner can access.
    Authorize,
}

impl fmt::Display for AccessModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AccessModel::Open => "open",
            AccessModel::Presence => "presence",
            AccessModel::Roster => "roster",
            AccessModel::Whitelist => "whitelist",
            AccessModel::Authorize => "authorize",
        };
        write!(f, "{s}")
    }
}

impl FromStr for AccessModel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" => Ok(AccessModel::Open),
            "presence" => Ok(AccessModel::Presence),
            "roster" => Ok(AccessModel::Roster),
            "whitelist" => Ok(AccessModel::Whitelist),
            "authorize" => Ok(AccessModel::Authorize),
            _ => Err(()),
        }
    }
}

/// Publish model for a PubSub node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PublishModel {
    /// Only node owners can publish.
    Publishers,
    /// Only subscribers can publish.
    Subscribers,
    /// Anyone can publish.
    #[default]
    Open,
}

impl fmt::Display for PublishModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PublishModel::Publishers => "publishers",
            PublishModel::Subscribers => "subscribers",
            PublishModel::Open => "open",
        };
        write!(f, "{s}")
    }
}

impl FromStr for PublishModel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "publishers" => Ok(PublishModel::Publishers),
            "subscribers" => Ok(PublishModel::Subscribers),
            "open" => Ok(PublishModel::Open),
            _ => Err(()),
        }
    }
}

/// When to send the last published item to a subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SendLastPublishedItem {
    /// Never send the last item automatically.
    Never,
    /// Send on subscription only.
    OnSub,
    /// Send on subscription and when the contact becomes available.
    #[default]
    OnSubAndPresence,
}

impl fmt::Display for SendLastPublishedItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SendLastPublishedItem::Never => "never",
            SendLastPublishedItem::OnSub => "on_sub",
            SendLastPublishedItem::OnSubAndPresence => "on_sub_and_presence",
        };
        write!(f, "{s}")
    }
}

impl FromStr for SendLastPublishedItem {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "never" => Ok(SendLastPublishedItem::Never),
            "on_sub" => Ok(SendLastPublishedItem::OnSub),
            "on_sub_and_presence" => Ok(SendLastPublishedItem::OnSubAndPresence),
            _ => Err(()),
        }
    }
}

/// Shared node configuration for PubSub / PEP nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeConfig {
    pub access_model: AccessModel,
    pub publish_model: PublishModel,
    pub max_items: u32,
    pub persist_items: bool,
    pub deliver_payloads: bool,
    pub notify_retract: bool,
    pub notify_delete: bool,
    pub send_last_published_item: SendLastPublishedItem,
}

/// Partial node configuration submitted by an XEP-0060 configure form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeConfigPatch {
    pub access_model: Option<AccessModel>,
    pub publish_model: Option<PublishModel>,
    pub max_items: Option<u32>,
    pub persist_items: Option<bool>,
    pub deliver_payloads: Option<bool>,
    pub notify_retract: Option<bool>,
    pub notify_delete: Option<bool>,
    pub send_last_published_item: Option<SendLastPublishedItem>,
}

impl NodeConfigPatch {
    pub fn apply_to(&self, mut config: NodeConfig) -> NodeConfig {
        if let Some(access_model) = self.access_model {
            config.access_model = access_model;
        }
        if let Some(publish_model) = self.publish_model {
            config.publish_model = publish_model;
        }
        if let Some(max_items) = self.max_items {
            config.max_items = max_items;
        }
        if let Some(persist_items) = self.persist_items {
            config.persist_items = persist_items;
        }
        if let Some(deliver_payloads) = self.deliver_payloads {
            config.deliver_payloads = deliver_payloads;
        }
        if let Some(notify_retract) = self.notify_retract {
            config.notify_retract = notify_retract;
        }
        if let Some(notify_delete) = self.notify_delete {
            config.notify_delete = notify_delete;
        }
        if let Some(send_last_published_item) = self.send_last_published_item {
            config.send_last_published_item = send_last_published_item;
        }
        config
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::pep_default()
    }
}

impl NodeConfig {
    /// XEP-0503 configuration for a public Space node.
    pub fn spaces_public() -> Self {
        Self {
            access_model: AccessModel::Open,
            publish_model: PublishModel::Publishers,
            max_items: u32::MAX,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSub,
        }
    }

    /// XEP-0503 configuration for a private Space node.
    pub fn spaces_private() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            max_items: u32::MAX,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSub,
        }
    }

    /// Default configuration for PEP nodes.
    pub fn pep_default() -> Self {
        Self {
            access_model: AccessModel::Presence,
            publish_model: PublishModel::Publishers,
            max_items: 1,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSubAndPresence,
        }
    }

    /// Default configuration for a concrete PEP node.
    pub fn pep_for_node(node: &str) -> Self {
        let mut config = Self::pep_default();
        if node == super::pep::PEP_NODE_BOOKMARKS {
            config.max_items = PEP_BOOKMARK_MAX_ITEMS;
            config = config.normalize_xep0402_bookmarks();
        } else if node == super::pep::PEP_NODE_MDS_DISPLAYED {
            config = Self::mds_displayed();
        } else if node == super::pep::PEP_NODE_VCARD4 {
            config = Self::vcard4_defaults();
        } else if node == super::pep::PEP_NODE_WADDLE_DND {
            config = Self::waddle_dnd_defaults();
        }
        config
    }

    /// Defaults for the Waddle DND PEP node (issue #367).
    ///
    /// The payload contains personally-identifying schedule data
    /// (timezone, sleep hours, snooze deadlines). The authoritative
    /// consumer is the server-side projection consulted at the T1
    /// push gate — no roster contact needs to read this node. Force
    /// `whitelist` access and `never` send-last-published so a fresh
    /// roster subscription does NOT receive the user's DND state.
    /// `max_items = 1` keeps the node single-slot (the PEP idiom for
    /// single-item nodes is `id = current`).
    ///
    /// `notify_retract` / `notify_delete` are set to `false` because
    /// the server does not currently emit the XEP-0060 §7.2.2 /
    /// §9.1.5 retract/delete events on the wire — advertising them
    /// as `true` would lie to subscribers. Bob's other resources
    /// resync DND state via `<items/>` GET on resume, not via
    /// retract fanout. See PR #759 / round-6 XMPP-conformance review.
    pub fn waddle_dnd_defaults() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            max_items: 1,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: false,
            notify_delete: false,
            send_last_published_item: SendLastPublishedItem::Never,
        }
    }

    /// XEP-0292 §6.1 vCard4 PEP node defaults.
    ///
    /// XEP-0292 §6.1 pins `open` as the canonical access model for the
    /// `urn:xmpp:vcard4` node so any peer (roster relationship or not)
    /// can resolve a user's published vCard4. `max_items = 1` keeps the
    /// node single-slot so each publish replaces the prior one rather
    /// than growing an unbounded item history — matching the
    /// OIDC-managed publisher in `waddle-server::profile::publish`.
    pub fn vcard4_defaults() -> Self {
        Self {
            access_model: AccessModel::Open,
            max_items: 1,
            ..Self::pep_default()
        }
    }

    /// XEP-0490 §3 Message Displayed Synchronization node defaults.
    ///
    /// The XEP mandates the publishing client send these as
    /// publish-options preconditions:
    ///   - `pubsub#persist_items=true`
    ///   - `pubsub#max_items=max`
    ///   - `pubsub#access_model=whitelist`
    ///   - `pubsub#send_last_published_item=never`
    ///
    /// We bake these into the well-known node defaults so auto-create
    /// matches the spec without depending on the (currently unenforced)
    /// publish-options precondition path. `max=max` maps to `u32::MAX`.
    pub fn mds_displayed() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            max_items: u32::MAX,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::Never,
        }
    }

    /// Force XEP-0402 bookmark privacy and durability invariants onto a config.
    pub fn normalize_xep0402_bookmarks(mut self) -> Self {
        self.access_model = AccessModel::Whitelist;
        self.publish_model = PublishModel::Publishers;
        if self.max_items == 0 || self.max_items > PEP_BOOKMARK_MAX_ITEMS {
            self.max_items = PEP_BOOKMARK_MAX_ITEMS;
        }
        self.persist_items = true;
        self.send_last_published_item = SendLastPublishedItem::Never;
        self
    }

    /// Configuration for a public node.
    pub fn public() -> Self {
        Self {
            access_model: AccessModel::Open,
            publish_model: PublishModel::Publishers,
            max_items: 10,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSub,
        }
    }

    /// Configuration for a whitelist-only node.
    pub fn whitelist() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            max_items: 10,
            persist_items: true,
            deliver_payloads: true,
            notify_retract: true,
            notify_delete: true,
            send_last_published_item: SendLastPublishedItem::OnSub,
        }
    }

    /// XEP-0357 Push Service node defaults.
    ///
    /// A Push Service node is a delivery target, not a subscriber-visible feed:
    /// use the XEP-0357 recommended whitelist access model and publisher-gated
    /// writes, keep only a bounded durable item trail, and never replay the last
    /// push notification to subscribers.
    pub fn push_service() -> Self {
        Self {
            access_model: AccessModel::Whitelist,
            publish_model: PublishModel::Publishers,
            max_items: 10_000,
            persist_items: true,
            deliver_payloads: false,
            notify_retract: false,
            notify_delete: false,
            send_last_published_item: SendLastPublishedItem::Never,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_model_round_trips() {
        assert_eq!("open".parse::<AccessModel>(), Ok(AccessModel::Open));
        assert_eq!("presence".parse::<AccessModel>(), Ok(AccessModel::Presence));
        assert_eq!(AccessModel::Whitelist.to_string(), "whitelist");
    }

    #[test]
    fn publish_model_round_trips() {
        assert_eq!(
            "publishers".parse::<PublishModel>(),
            Ok(PublishModel::Publishers)
        );
        assert_eq!(PublishModel::Open.to_string(), "open");
    }

    #[test]
    fn node_config_defaults_match_pep() {
        let config = NodeConfig::default();
        assert_eq!(config, NodeConfig::pep_default());
        assert_eq!(config.max_items, 1);
        assert!(config.persist_items);
    }

    #[test]
    fn bookmarks_pep_config_keeps_multiple_private_items() {
        let config = NodeConfig::pep_for_node(super::super::pep::PEP_NODE_BOOKMARKS);
        assert_eq!(config.access_model, AccessModel::Whitelist);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert_eq!(config.max_items, PEP_BOOKMARK_MAX_ITEMS);
        assert!(config.persist_items);
        assert_eq!(
            config.send_last_published_item,
            SendLastPublishedItem::Never
        );
    }

    #[test]
    fn bookmarks_normalization_preserves_bounded_retention() {
        let mut config = NodeConfig::spaces_public();
        config.max_items = 10;

        let normalized = config.normalize_xep0402_bookmarks();

        assert_eq!(normalized.access_model, AccessModel::Whitelist);
        assert_eq!(normalized.publish_model, PublishModel::Publishers);
        assert_eq!(normalized.max_items, 10);
        assert!(normalized.persist_items);
        assert_eq!(
            normalized.send_last_published_item,
            SendLastPublishedItem::Never
        );
    }

    #[test]
    fn spaces_configs_do_not_inherit_pep_single_item_retention() {
        let public = NodeConfig::spaces_public();
        assert_eq!(public.access_model, AccessModel::Open);
        assert_eq!(public.max_items, u32::MAX);
        assert!(public.persist_items);
        assert!(public.notify_retract);

        let private = NodeConfig::spaces_private();
        assert_eq!(private.access_model, AccessModel::Whitelist);
        assert_eq!(private.max_items, u32::MAX);
        assert!(private.persist_items);
        assert!(private.notify_retract);
    }

    #[test]
    fn vcard4_pep_config_is_open_with_single_item_retention() {
        // XEP-0292 §6.1 canonical access model: `open`. The auto-create
        // path on a publish to `urn:xmpp:vcard4` MUST land Open, not
        // Presence — otherwise non-roster peers can't read the vCard
        // even though the XEP says the node is publicly readable.
        let config = NodeConfig::pep_for_node(super::super::pep::PEP_NODE_VCARD4);
        assert_eq!(config.access_model, AccessModel::Open);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert_eq!(config.max_items, 1);
        assert!(config.persist_items);
    }

    #[test]
    fn vcard4_defaults_helper_matches_pep_for_node() {
        // Defence-in-depth: `vcard4_defaults()` is the single source of
        // truth that the publish-handler reconcile path will compare
        // existing-node configs against, so `pep_for_node` MUST return
        // the same value.
        assert_eq!(
            NodeConfig::pep_for_node(super::super::pep::PEP_NODE_VCARD4),
            NodeConfig::vcard4_defaults()
        );
    }

    #[test]
    fn push_service_config_matches_xep0357_delivery_target_defaults() {
        let config = NodeConfig::push_service();

        assert_eq!(config.access_model, AccessModel::Whitelist);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert!(config.persist_items);
        assert!(!config.deliver_payloads);
        assert_eq!(
            config.send_last_published_item,
            SendLastPublishedItem::Never
        );
    }
}
