//! PubSub node configuration.
//!
//! Defines node configuration options including access models and publish models.

use std::fmt;

/// Access model for a PubSub node.
///
/// Determines who can subscribe to and retrieve items from the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessModel {
    /// Anyone can subscribe and retrieve items.
    Open,
    /// Only entities in the owner's roster with a subscription can access.
    /// This is the default for PEP nodes.
    #[default]
    Presence,
    /// Only entities in specific roster groups can access.
    Roster,
    /// Only explicitly whitelisted JIDs can access.
    Whitelist,
    /// Only the node owner can access (for private storage).
    Authorize,
}

impl AccessModel {
    /// Parse an access model from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(AccessModel::Open),
            "presence" => Some(AccessModel::Presence),
            "roster" => Some(AccessModel::Roster),
            "whitelist" => Some(AccessModel::Whitelist),
            "authorize" => Some(AccessModel::Authorize),
            _ => None,
        }
    }
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
        write!(f, "{}", s)
    }
}

/// Publish model for a PubSub node.
///
/// Determines who can publish to the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublishModel {
    /// Only node owners can publish.
    Publishers,
    /// Only subscribers can publish.
    Subscribers,
    /// Anyone can publish (typical for PEP).
    #[default]
    Open,
}

impl PublishModel {
    /// Parse a publish model from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "publishers" => Some(PublishModel::Publishers),
            "subscribers" => Some(PublishModel::Subscribers),
            "open" => Some(PublishModel::Open),
            _ => None,
        }
    }
}

impl fmt::Display for PublishModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PublishModel::Publishers => "publishers",
            PublishModel::Subscribers => "subscribers",
            PublishModel::Open => "open",
        };
        write!(f, "{}", s)
    }
}

/// Configuration for a PubSub node.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Access model (who can subscribe/retrieve).
    pub access_model: AccessModel,
    /// Publish model (who can publish).
    pub publish_model: PublishModel,
    /// Maximum number of items to persist (0 = unlimited, 1 = typical for PEP).
    pub max_items: u32,
    /// Whether to persist items across server restarts.
    pub persist_items: bool,
    /// Whether to deliver payloads in notifications.
    pub deliver_payloads: bool,
    /// Whether to notify on item retraction.
    pub notify_retract: bool,
    /// Whether to notify on node deletion.
    pub notify_delete: bool,
    /// Whether to send last published item on subscription.
    pub send_last_published_item: SendLastPublishedItem,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::pep_default()
    }
}

impl NodeConfig {
    /// Default configuration for PEP nodes (XEP-0163).
    ///
    /// - Access model: presence (roster-based)
    /// - Max items: 1 (only latest item kept)
    /// - Persist items: true
    /// - Deliver payloads: true
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

    /// Configuration for a public node (anyone can subscribe).
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
}

/// When to send the last published item to a subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SendLastPublishedItem {
    /// Never send last item automatically.
    Never,
    /// Send on subscription only.
    OnSub,
    /// Send on subscription and when contact comes online (PEP default).
    #[default]
    OnSubAndPresence,
}

impl SendLastPublishedItem {
    /// Parse from a string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "never" => Some(SendLastPublishedItem::Never),
            "on_sub" => Some(SendLastPublishedItem::OnSub),
            "on_sub_and_presence" => Some(SendLastPublishedItem::OnSubAndPresence),
            _ => None,
        }
    }
}

impl fmt::Display for SendLastPublishedItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SendLastPublishedItem::Never => "never",
            SendLastPublishedItem::OnSub => "on_sub",
            SendLastPublishedItem::OnSubAndPresence => "on_sub_and_presence",
        };
        write!(f, "{}", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_access_model_parse() {
        assert_eq!(AccessModel::from_str("open"), Some(AccessModel::Open));
        assert_eq!(AccessModel::from_str("presence"), Some(AccessModel::Presence));
        assert_eq!(AccessModel::from_str("roster"), Some(AccessModel::Roster));
        assert_eq!(AccessModel::from_str("whitelist"), Some(AccessModel::Whitelist));
        assert_eq!(AccessModel::from_str("invalid"), None);
    }

    #[test]
    fn test_access_model_display() {
        assert_eq!(AccessModel::Open.to_string(), "open");
        assert_eq!(AccessModel::Presence.to_string(), "presence");
    }

    #[test]
    fn test_publish_model_parse() {
        assert_eq!(PublishModel::from_str("publishers"), Some(PublishModel::Publishers));
        assert_eq!(PublishModel::from_str("open"), Some(PublishModel::Open));
        assert_eq!(PublishModel::from_str("invalid"), None);
    }

    #[test]
    fn test_node_config_defaults() {
        let config = NodeConfig::default();
        assert_eq!(config.access_model, AccessModel::Presence);
        assert_eq!(config.max_items, 1);
        assert!(config.persist_items);
    }

    #[test]
    fn test_pep_default() {
        let config = NodeConfig::pep_default();
        assert_eq!(config.access_model, AccessModel::Presence);
        assert_eq!(config.publish_model, PublishModel::Publishers);
        assert_eq!(config.max_items, 1);
    }
}
