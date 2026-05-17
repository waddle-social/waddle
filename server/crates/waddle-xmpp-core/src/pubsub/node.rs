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
        }
        config
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
