//! XEP-0108: User Activity (PEP activity) dedicated suite.

use waddle_xmpp::pubsub::{AccessModel, PepHandler};

const ACTIVITY_NODE: &str = "http://jabber.org/protocol/activity";

#[test]
fn xep0108_well_known_node_is_supported() {
    assert!(PepHandler::is_well_known_node(ACTIVITY_NODE));
}

#[test]
fn xep0108_unknown_variant_is_rejected() {
    assert!(!PepHandler::is_well_known_node(
        "http://jabber.org/protocol/activity/unsupported"
    ));
}

#[test]
fn xep0108_access_model_consistency() {
    assert_eq!(
        PepHandler::default_access_model_for_node(ACTIVITY_NODE),
        AccessModel::Presence
    );
}
