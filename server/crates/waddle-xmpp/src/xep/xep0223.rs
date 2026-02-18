//! XEP-0223: Persistent Storage of Private Data via PubSub
//!
//! This is a profile of PubSub (XEP-0060), not a standalone protocol.
//! It describes best practices for using PubSub to store private data
//! with access_model=whitelist.
//!
//! The implementation ensures PEP nodes can be configured with whitelist
//! access for private data (e.g., bookmarks, OMEMO keys).

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
    // OMEMO bundles (private keys)
    || node.starts_with("eu.siacs.conversations.axolotl.bundles")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_private_storage_node() {
        assert!(is_private_storage_node("urn:xmpp:bookmarks:1"));
        assert!(is_private_storage_node("storage:bookmarks"));
        assert!(is_private_storage_node(
            "eu.siacs.conversations.axolotl.bundles:12345"
        ));
        assert!(!is_private_storage_node("urn:xmpp:avatar:data"));
        assert!(!is_private_storage_node("some:custom:node"));
    }
}
