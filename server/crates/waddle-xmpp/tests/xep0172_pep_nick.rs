//! XEP-0172: User Nickname (PEP nick) dedicated suite.

use waddle_xmpp::pubsub::{AccessModel, PepHandler};

const NICK_NODE: &str = "http://jabber.org/protocol/nick";

#[test]
fn xep0172_well_known_node_is_supported() {
    assert!(PepHandler::is_well_known_node(NICK_NODE));
}

#[test]
fn xep0172_unknown_variant_is_rejected() {
    assert!(!PepHandler::is_well_known_node(
        "http://jabber.org/protocol/nickname"
    ));
}

#[test]
fn xep0172_access_model_consistency() {
    assert_eq!(
        PepHandler::default_access_model_for_node(NICK_NODE),
        AccessModel::Presence
    );
}
