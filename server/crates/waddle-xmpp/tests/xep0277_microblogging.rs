//! XEP-0277: Microblogging over PEP dedicated suite.

use waddle_xmpp::pubsub::{AccessModel, PepHandler};

const MICROBLOG_NODE: &str = "urn:xmpp:microblog:0";

#[test]
fn xep0277_well_known_node_is_supported() {
    assert!(PepHandler::is_well_known_node(MICROBLOG_NODE));
}

#[test]
fn xep0277_unknown_variant_is_rejected() {
    assert!(!PepHandler::is_well_known_node("urn:xmpp:microblog:1"));
}

#[test]
fn xep0277_access_model_consistency() {
    assert_eq!(
        PepHandler::default_access_model_for_node(MICROBLOG_NODE),
        AccessModel::Presence
    );
}
