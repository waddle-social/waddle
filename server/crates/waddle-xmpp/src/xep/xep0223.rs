//! XEP-0223: Persistent Storage of Private Data via PubSub
//!
//! This is a profile of PubSub (XEP-0060), not a standalone protocol.
//! It describes best practices for using PubSub to store private data
//! with access_model=whitelist.

/// Namespace constant for PubSub access whitelist.
pub const FEATURE_ACCESS_WHITELIST: &str = "http://jabber.org/protocol/pubsub#access-whitelist";

/// Namespace constant for PubSub persistent items.
pub const FEATURE_PERSISTENT_ITEMS: &str = "http://jabber.org/protocol/pubsub#persistent-items";

/// Check if a node name is typically stored with whitelist access per XEP-0223.
///
/// These are nodes that contain private data and should only be accessible
/// to the node owner.
pub fn is_private_storage_node(node: &str) -> bool {
    // Bookmarks (XEP-0402) - private by default
    node == "urn:xmpp:bookmarks:1"
    // Legacy bookmarks
    || node == "storage:bookmarks"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_private_storage_node() {
        assert!(is_private_storage_node("urn:xmpp:bookmarks:1"));
        assert!(is_private_storage_node("storage:bookmarks"));
        assert!(!is_private_storage_node("urn:xmpp:avatar:data"));
        assert!(!is_private_storage_node("some:custom:node"));
    }

    #[test]
    fn test_feature_constants() {
        assert_eq!(
            FEATURE_ACCESS_WHITELIST,
            "http://jabber.org/protocol/pubsub#access-whitelist"
        );
        assert_eq!(
            FEATURE_PERSISTENT_ITEMS,
            "http://jabber.org/protocol/pubsub#persistent-items"
        );
    }

    #[test]
    fn test_empty_string_not_private() {
        assert!(!is_private_storage_node(""));
    }

    #[test]
    fn test_pep_nodes_not_private() {
        assert!(!is_private_storage_node("urn:xmpp:avatar:data"));
        assert!(!is_private_storage_node("urn:xmpp:avatar:metadata"));
        assert!(!is_private_storage_node("urn:xmpp:notification-settings:1"));
    }
}
