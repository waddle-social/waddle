//! XEP-0107: User Mood (PEP mood) dedicated suite.

use waddle_xmpp::pubsub::{AccessModel, PepHandler};

const MOOD_NODE: &str = "http://jabber.org/protocol/mood";

#[test]
fn xep0107_well_known_node_is_supported() {
    assert!(PepHandler::is_well_known_node(MOOD_NODE));
}

#[test]
fn xep0107_unknown_variant_is_rejected() {
    assert!(!PepHandler::is_well_known_node(
        "http://jabber.org/protocol/mood/unsupported"
    ));
}

#[test]
fn xep0107_access_model_consistency() {
    assert_eq!(
        PepHandler::default_access_model_for_node(MOOD_NODE),
        AccessModel::Presence
    );
}
