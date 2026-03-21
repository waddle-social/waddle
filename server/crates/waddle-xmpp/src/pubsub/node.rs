//! PubSub node configuration.
//!
//! Defines node configuration options including access models and publish models.

use std::{fmt, str::FromStr};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_access_model_parse() {
        assert_eq!("open".parse::<AccessModel>(), Ok(AccessModel::Open));
        assert_eq!("presence".parse::<AccessModel>(), Ok(AccessModel::Presence));
        assert_eq!("roster".parse::<AccessModel>(), Ok(AccessModel::Roster));
        assert_eq!(
            "whitelist".parse::<AccessModel>(),
            Ok(AccessModel::Whitelist)
        );
        assert!("invalid".parse::<AccessModel>().is_err());
    }

    #[test]
    fn test_access_model_display() {
        assert_eq!(AccessModel::Open.to_string(), "open");
        assert_eq!(AccessModel::Presence.to_string(), "presence");
    }

    #[test]
    fn test_publish_model_parse() {
        assert_eq!(
            "publishers".parse::<PublishModel>(),
            Ok(PublishModel::Publishers)
        );
        assert_eq!("open".parse::<PublishModel>(), Ok(PublishModel::Open));
        assert!("invalid".parse::<PublishModel>().is_err());
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
