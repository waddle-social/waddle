//! XEP-0402: Bookmarks over PEP dedicated suite.

use jid::BareJid;
use waddle_xmpp::pubsub::{AccessModel, InMemoryPubSubStorage, PepHandler, PubSubStorage};

const BOOKMARKS_NODE: &str = "urn:xmpp:bookmarks:1";

#[test]
fn xep0402_bookmarks_default_access_model_is_private() {
    assert_eq!(
        PepHandler::default_access_model_for_node(BOOKMARKS_NODE),
        AccessModel::Whitelist
    );
}

#[tokio::test]
async fn xep0402_auto_created_bookmarks_node_uses_private_defaults() {
    let storage = InMemoryPubSubStorage::new();
    let owner: BareJid = "user@example.com".parse().expect("valid jid");

    let (node, created) = storage
        .get_or_create_node(&owner, BOOKMARKS_NODE)
        .await
        .expect("bookmarks node should be created");

    assert!(created);
    assert_eq!(node.config.access_model, AccessModel::Whitelist);
    assert_eq!(node.config.max_items, 1);
}
