//! XEP-0118: User Tune (PEP tune) dedicated suite.

use waddle_xmpp::pubsub::{AccessModel, PepHandler};

const TUNE_NODE: &str = "http://jabber.org/protocol/tune";

#[test]
fn xep0118_well_known_node_is_supported() {
    assert!(PepHandler::is_well_known_node(TUNE_NODE));
}

#[test]
fn xep0118_unknown_variant_is_rejected() {
    assert!(!PepHandler::is_well_known_node(
        "http://jabber.org/protocol/tunes"
    ));
}

#[test]
fn xep0118_access_model_consistency() {
    assert_eq!(
        PepHandler::default_access_model_for_node(TUNE_NODE),
        AccessModel::Presence
    );
}
