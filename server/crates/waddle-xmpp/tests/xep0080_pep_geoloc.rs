//! XEP-0080: User Location (PEP geoloc) dedicated suite.

use waddle_xmpp::pubsub::{AccessModel, PepHandler};

const GEOLOC_NODE: &str = "http://jabber.org/protocol/geoloc";

#[test]
fn xep0080_well_known_node_is_supported() {
    assert!(PepHandler::is_well_known_node(GEOLOC_NODE));
}

#[test]
fn xep0080_unknown_variant_is_rejected() {
    assert!(!PepHandler::is_well_known_node(
        "http://jabber.org/protocol/geoloc2"
    ));
}

#[test]
fn xep0080_access_model_consistency() {
    assert_eq!(
        PepHandler::default_access_model_for_node(GEOLOC_NODE),
        AccessModel::Presence
    );
}
